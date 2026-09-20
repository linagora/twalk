//! One poll of the owner's calendars (issue #280): the I/O the pure
//! `caldav` module is wrapped in — the side service asked, the cursor per
//! calendar read from and written to the state directory, the consent
//! decision read on the mail connection — kept apart from publishing so the
//! cursor moves only after the bus took the events. A poll that finds the
//! CTag unchanged asks nothing further and publishes nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::{info, warn};
use twalk_consent_cache::{Consent, ConsentCache};

use crate::caldav::{self, Cursor, Envelopes, Known, Side, SideError};

/// The calendar connection this process holds, and what publishing about it
/// needs.
pub struct Calendars {
    pub connection: String,
    pub owner_email: String,
    /// The mail connection of the same account, whose decisions govern the
    /// participants; `None` on a deployment with no mail connection, where
    /// nobody is ever withheld because nobody was ever decided about.
    pub mail_connection: Option<String>,
    pub side: Side,
    pub state_dir: PathBuf,
    pub consent: ConsentCache,
}

/// What one poll found: the envelopes to publish, in order, and the cursors
/// to write once they are on the bus.
#[derive(Debug, Default)]
pub struct Poll {
    pub envelopes: Vec<Value>,
    pub cursors: Vec<(String, Cursor)>,
}

impl Calendars {
    fn cursor_path(&self, calendar_id: &str) -> PathBuf {
        self.state_dir
            .join("caldav")
            .join(&self.connection)
            .join(format!("{calendar_id}.json"))
    }

    fn read_cursor(&self, calendar_id: &str) -> Result<Cursor> {
        let path = self.cursor_path(calendar_id);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).with_context(|| {
                format!("{} is not a cursor this collector wrote", path.display())
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Cursor::default()),
            Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Writes the cursors a poll handed back, after its envelopes were
    /// published: by rename, so a crash leaves the previous cursor whole and
    /// the next poll republishes what the bus already deduplicates.
    pub fn commit(&self, cursors: &[(String, Cursor)]) -> Result<()> {
        for (calendar_id, cursor) in cursors {
            write_json(&self.cursor_path(calendar_id), cursor)?;
        }
        Ok(())
    }

    /// The decision about a `mailto:` on the mail connection, or `Pending`
    /// — labelled by nothing, reduced by nothing — when there is none.
    fn decide(&self, identity: &str) -> Consent {
        match &self.mail_connection {
            Some(mail) => self.consent.state(identity, mail),
            None => Consent::Pending,
        }
    }

    /// One poll: every calendar of the owner's, its CTag against the cursor,
    /// and for the ones that moved the listing, the diff and the reads.
    pub async fn poll(&self, owner_id: &str, token: &str, now: &str) -> Result<Poll, SideError> {
        let mut poll = Poll::default();
        let calendars = self.side.calendars(owner_id, token).await?;
        for calendar in calendars {
            let collection = caldav::collection_path(owner_id, &calendar.id);
            let cursor = match self.read_cursor(&calendar.id) {
                Ok(cursor) => cursor,
                Err(error) => {
                    // A cursor this collector cannot read is not one to
                    // guess past: said, and the calendar skipped this poll.
                    warn!(calendar = %calendar.id, error = %format!("{error:#}"), "the calendar's cursor cannot be read; skipping it");
                    continue;
                }
            };
            let listing = self.side.listing(&collection, token).await?;
            if listing.ctag.is_some() && listing.ctag == cursor.ctag {
                continue;
            }
            let changes = caldav::diff(&cursor, &listing);
            let envelopes = Envelopes::new(
                &self.connection,
                &self.owner_email,
                &self.side.host(),
                &collection,
            );
            let mut next = Cursor {
                ctag: listing.ctag.clone(),
                known: cursor.known.clone(),
            };
            let resources = self
                .side
                .read(&collection, &changes.to_read(), token)
                .await?;
            let by_href: BTreeMap<&str, &caldav::Resource> = resources
                .iter()
                .map(|resource| (resource.href.as_str(), resource))
                .collect();
            for href in &changes.created {
                let Some(resource) = by_href.get(href.as_str()) else {
                    continue; // gone between the listing and the read
                };
                match self.publishable(resource) {
                    Some(published) => {
                        poll.envelopes.push(envelopes.created(
                            href,
                            &resource.etag,
                            &published,
                            now,
                        ));
                        next.known.insert(
                            href.clone(),
                            Known {
                                etag: resource.etag.clone(),
                                published,
                            },
                        );
                    }
                    None => {
                        // Remembered as seen, so an unreadable resource is
                        // not re-read at every poll; nothing published.
                        next.known.insert(
                            href.clone(),
                            Known {
                                etag: resource.etag.clone(),
                                published: Value::Null,
                            },
                        );
                    }
                }
            }
            for href in &changes.changed {
                let Some(resource) = by_href.get(href.as_str()) else {
                    continue;
                };
                let before = cursor.known.get(href).map(|known| &known.published);
                match self.publishable(resource) {
                    Some(published) => {
                        let fields = match before {
                            Some(before) if !before.is_null() => {
                                caldav::changed_fields(before, &published)
                            }
                            // Never published before (unreadable then): now
                            // it is, and it is new to every consumer.
                            _ => Vec::new(),
                        };
                        if before.is_some_and(|before| !before.is_null()) {
                            if !fields.is_empty() {
                                poll.envelopes.push(envelopes.changed(
                                    href,
                                    &resource.etag,
                                    &published,
                                    &fields,
                                    now,
                                ));
                            } else {
                                info!(
                                    href,
                                    "the resource changed in nothing the contract publishes"
                                );
                            }
                        } else {
                            poll.envelopes.push(envelopes.created(
                                href,
                                &resource.etag,
                                &published,
                                now,
                            ));
                        }
                        next.known.insert(
                            href.clone(),
                            Known {
                                etag: resource.etag.clone(),
                                published,
                            },
                        );
                    }
                    None => {
                        if let Some(known) = next.known.get_mut(href) {
                            known.etag = resource.etag.clone();
                        }
                    }
                }
            }
            for href in &changes.removed {
                if let Some(known) = cursor.known.get(href) {
                    if let (Some(uid), Some(title)) = (
                        known.published.get("uid").and_then(Value::as_str),
                        known.published.get("title").and_then(Value::as_str),
                    ) {
                        poll.envelopes.push(envelopes.removed(
                            href,
                            listing.ctag.as_deref().unwrap_or_default(),
                            uid,
                            title,
                            now,
                        ));
                    }
                }
                next.known.remove(href);
            }
            poll.cursors.push((calendar.id.clone(), next));
        }
        Ok(poll)
    }

    /// The resource as the contract publishes it, or `None` — said — for
    /// one that is not a VEVENT this collector can read.
    fn publishable(&self, resource: &caldav::Resource) -> Option<Value> {
        match caldav::parse_vevent(&resource.ics) {
            Ok(event) => Some(caldav::reduce(&event, &self.owner_email, |identity| {
                self.decide(identity)
            })),
            Err(error) => {
                warn!(href = %resource.href, error = %format!("{error:#}"), "the resource is not a VEVENT this collector reads; nothing published");
                None
            }
        }
    }
}

/// A JSON file written by rename, mode 0600 in a 0700 directory, the way
/// the grant is.
fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let directory = path.parent().context("the cursor file has no parent")?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set the mode of {}", directory.display()))?;
    let temporary = directory.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cursor.json")
    ));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("failed to open {}", temporary.display()))?;
        file.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)
        .with_context(|| format!("failed to move {} into place", temporary.display()))?;
    Ok(())
}
