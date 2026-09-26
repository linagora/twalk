//! Reading a time-zone name that came from somebody else's calendar.
//!
//! An iCalendar `TZID` is whatever the client that wrote the event put there,
//! and the two families in the wild are not the same:
//!
//! - **IANA** names — `Europe/Paris` — which `chrono_tz` reads directly;
//! - **Windows** names — `Romance Standard Time` — which Outlook writes, and
//!   which are not zones at all in the IANA sense but labels of Microsoft's own
//!   table.
//!
//! Measured on the reference deployment on 2026-09-24, in one window: 57 events
//! with `Romance Standard Time`, two with `W. Europe Standard Time`, one with
//! `GMT Standard Time` — sixty of the owner's real meetings, refused and never
//! published, which made their free/busy a description of a calendar they do
//! not have ([#350](https://github.com/linagora/twalk/issues/350)). A corporate
//! calendar is mostly such events.
//!
//! So this module reads both, and **nothing else**. The mapping is not ours to
//! invent: it is CLDR's `windowsZones.xml`, the table every calendar client
//! uses, generated into [`crate::windows_zones`] with its provenance recorded.
//! A name in neither family is still refused, and the refusal still says which
//! name it was — an instant this collector cannot place is one it must not
//! guess at, and that rule is the reason the sixty events were noticed at all
//! rather than published an hour wrong.
//!
//! Everything downstream sees an IANA zone or nothing. That is deliberate and
//! is the reason this is one function rather than a patch at the one call site
//! that was failing: the zone of the owner's own events is *stored* (#376) and
//! then read again to spell their local hours (#369) and to clip their working
//! day (#381), and a Windows name travelling into that path would break three
//! things quietly instead of one loudly.

use std::fmt;

use crate::windows_zones::WINDOWS_ZONES;

/// The zone a `TZID` means, or `None` when this collector cannot place it.
///
/// IANA first, because it is the form the standard asks for and the form most
/// calendars write; the Windows table second, because it is the form the rest
/// of them write.
pub fn read(name: &str) -> Option<chrono_tz::Tz> {
    let name = name.trim();
    if let Ok(zone) = name.parse::<chrono_tz::Tz>() {
        return Some(zone);
    }
    let index = WINDOWS_ZONES
        .binary_search_by(|(windows, _)| (*windows).cmp(name))
        .ok()?;
    // A CLDR row this build of `chrono_tz` cannot read is a `None` and not a
    // panic: the table is data from outside, and the test below is what keeps
    // that case theoretical.
    WINDOWS_ZONES[index].1.parse().ok()
}

/// A `TZID` this collector cannot place, and which of the two ways it could
/// not be.
///
/// An error type rather than a message, so that the thing which *counts* the
/// refusal and the thing which *logs* it read the same value. The alternative
/// — one of them matching on the other's prose — is how a metric starts
/// disagreeing with a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unplaceable {
    /// What the calendar wrote, verbatim.
    pub tzid: String,
    /// Whether CLDR knows the name and this build's zone database does not —
    /// which is "your calendar speaks Windows and this build's table is older
    /// than it", a different sentence from "that is not a zone".
    pub windows: bool,
}

impl Unplaceable {
    /// The reason a counter carries, and the only two values it takes.
    pub const REASONS: [&'static str; 2] = ["zone_unknown", "zone_windows_unmappable"];

    pub fn reason(&self) -> &'static str {
        if self.windows {
            "zone_windows_unmappable"
        } else {
            "zone_unknown"
        }
    }
}

impl fmt::Display for Unplaceable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.windows {
            write!(
                formatter,
                "TZID {:?} is a Windows zone name this build's CLDR table maps to a zone its own \
                 database does not have",
                self.tzid
            )
        } else {
            write!(
                formatter,
                "TZID {:?} is neither an IANA zone nor a Windows zone name",
                self.tzid
            )
        }
    }
}

impl std::error::Error for Unplaceable {}

/// The zone a `TZID` means, or the refusal that says why not — the form
/// [`crate::caldav`] needs, since it both logs and counts it.
pub fn place(name: &str) -> Result<chrono_tz::Tz, Unplaceable> {
    read(name).ok_or_else(|| Unplaceable {
        tzid: name.trim().to_owned(),
        windows: WINDOWS_ZONES
            .binary_search_by(|(windows, _)| (*windows).cmp(name.trim()))
            .is_ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_names_this_deployment_met_are_read() {
        // The ones measured on 2026-09-24, with the zones a calendar client
        // shows for them.
        assert_eq!(read("Romance Standard Time"), Some(chrono_tz::Europe::Paris));
        assert_eq!(
            read("W. Europe Standard Time"),
            Some(chrono_tz::Europe::Berlin)
        );
        assert_eq!(
            read("GMT Standard Time"),
            Some(chrono_tz::Europe::London)
        );
    }

    #[test]
    fn an_iana_name_is_still_read_first() {
        assert_eq!(read("Europe/Paris"), Some(chrono_tz::Europe::Paris));
        assert_eq!(read("UTC"), Some(chrono_tz::UTC));
        assert_eq!(read(" Europe/Paris "), Some(chrono_tz::Europe::Paris));
    }

    #[test]
    fn a_refusal_says_which_of_the_two_ways_it_could_not_be_placed() {
        // The reason a counter carries comes from the same value the log line
        // does, so the two cannot disagree (#350).
        let unknown = place("Middle-earth Standard Time").expect_err("a refusal");
        assert_eq!(unknown.reason(), "zone_unknown");
        assert!(!unknown.windows);
        assert!(format!("{unknown}").contains("neither an IANA zone"));
        assert!(Unplaceable::REASONS.contains(&unknown.reason()));

        // And every name CLDR knows is placeable on this build, which is what
        // the guard below asserts — so the other reason is unreachable here
        // and is constructed rather than provoked.
        let unmappable = Unplaceable {
            tzid: "Romance Standard Time".to_owned(),
            windows: true,
        };
        assert_eq!(unmappable.reason(), "zone_windows_unmappable");
        assert!(format!("{unmappable}").contains("CLDR table"));

        assert!(place("Europe/Paris").is_ok());
    }

    #[test]
    fn a_name_in_neither_family_is_refused_rather_than_guessed() {
        // Including the shapes that look nearly right: the point of refusing
        // is that an event this collector cannot place is not published an
        // hour wrong.
        for unknown in [
            "Middle-earth Standard Time",
            "romance standard time",
            "Romance",
            "Europe/Pariss",
            "GMT+1",
            "",
        ] {
            assert_eq!(read(unknown), None, "{unknown:?}");
        }
    }

    #[test]
    fn every_zone_the_table_names_is_one_this_build_can_read() {
        // The guard on the data: CLDR names a zone, `chrono_tz` ships a zone
        // database, and a release of either could disagree. If this fails, the
        // table was regenerated against a CLDR release this build's zone
        // database does not cover, and the answer is to say which rows rather
        // than to discover it on somebody's calendar.
        let unreadable: Vec<&str> = WINDOWS_ZONES
            .iter()
            .filter(|(_, iana)| iana.parse::<chrono_tz::Tz>().is_err())
            .map(|(windows, _)| *windows)
            .collect();
        assert!(unreadable.is_empty(), "{unreadable:?}");
    }

    #[test]
    fn the_table_is_the_cldr_file_committed_beside_its_generator() {
        // The provenance in `windows_zones.rs`'s header is a claim; this is
        // what makes it checkable. The CLDR file is committed at
        // `collector/tools/windowsZones.xml`, and the rows are derived from it
        // again here — in Rust, so it runs in the suite that already runs —
        // rather than trusting that somebody ran the generator and edited
        // nothing afterwards.
        //
        // Updating CLDR is therefore: replace the XML, re-run
        // `collector/tools/generate-windows-zones.py`, and watch this test.
        let xml = include_str!("../tools/windowsZones.xml");
        let mut derived: Vec<(&str, &str)> = Vec::new();
        for element in xml.split("<mapZone ").skip(1) {
            let attribute = |name: &str| -> Option<&str> {
                let rest = element.split_once(&format!("{name}=\""))?.1;
                rest.split_once('"').map(|(value, _)| value)
            };
            let (Some(other), Some(territory), Some(kind)) =
                (attribute("other"), attribute("territory"), attribute("type"))
            else {
                panic!("a mapZone element with missing attributes: {element:.120}");
            };
            if territory == "001" {
                // CLDR writes one or more zones in `type`; the first is the
                // one the territory-001 row means.
                derived.push((other, kind.split(' ').next().unwrap_or(kind)));
            }
        }
        derived.sort_unstable();
        assert!(
            derived.len() > 100,
            "the committed CLDR file parsed as {} rows, which is not a windowsZones.xml",
            derived.len()
        );
        assert_eq!(
            derived,
            WINDOWS_ZONES.to_vec(),
            "src/windows_zones.rs is not what tools/windowsZones.xml says"
        );
    }

    #[test]
    fn the_table_is_sorted_because_the_lookup_is_a_binary_search() {
        let mut sorted: Vec<&str> = WINDOWS_ZONES.iter().map(|(windows, _)| *windows).collect();
        let given = sorted.clone();
        sorted.sort_unstable();
        assert_eq!(given, sorted);
        assert_eq!(
            sorted.len(),
            sorted.iter().collect::<std::collections::BTreeSet<_>>().len(),
            "a Windows name appears twice, so one of the two rows is unreachable"
        );
    }
}
