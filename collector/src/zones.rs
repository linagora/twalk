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

/// Whether a name is a Windows one — used only to say so in a log line, so an
/// operator reading a refusal can tell "your calendar speaks Windows and this
/// build's table is too old" from "that is not a zone".
pub fn is_windows_name(name: &str) -> bool {
    WINDOWS_ZONES
        .binary_search_by(|(windows, _)| (*windows).cmp(name.trim()))
        .is_ok()
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
