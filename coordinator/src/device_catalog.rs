//! Maps the model identifier z2m's `device_interview_successful` event
//! actually carries — `definition.model` — to the human-readable
//! product-line name auto-assigned the first time a device is
//! successfully interviewed while pairing. See
//! `plans/device-auto-naming.md`.
//!
//! `definition.model` is a vendor-specific *retail SKU* for most vendors
//! (e.g. Philips' "929003666501"), not the short internal code z2m's
//! `bridge/devices` dump calls `model_id` ("LCG006") — that field is only
//! present in the full device-list dump, never on the interview event
//! itself (confirmed against a live payload), so it can't be used here.
//! SONOFF happens to use the same short code for both.

use std::collections::HashMap;

/// `None` for a `definition.model` SKU we don't recognise — the device
/// just keeps showing its raw hex `device_id` until someone renames it by
/// hand.
pub fn product_line_name(definition_model: &str) -> Option<&'static str> {
    Some(match definition_model {
        "929003666501" => "Hue GU10 Spot CCT/COL",   // LCG006
        "8718696598283" => "Hue GU10 Spot CCT",      // LTW013
        "8719514392830" => "Hue Filament Globe CCT", // LTA005
        "9290012574" => "Hue Color Ambiance Bulb",   // LCT010
        "SNZB-02P" => "Sonoff Temp/Humidity Sensor",
        "SNZB-03PR2" => "Sonoff Motion Sensor",
        "8719514440937/8719514440999" => "Hue Tap Dial Switch", // RDM002
        "8718699693985" => "Hue Smart Button",                  // ROM001
        _ => return None,
    })
}

/// Next free number in `product_line`'s sequence, given the custom names
/// already assigned to *other* devices — e.g. `"Hue GU10 Spot CCT/COL 3"`
/// when "... 1" and "... 2" are already taken. Always numbers, even for a
/// product line's first device, so a second unit later never collides.
pub fn next_name_in_line(existing_names: &HashMap<String, String>, product_line: &str) -> String {
    let next_n = existing_names
        .values()
        .filter_map(|name| name.strip_prefix(product_line)?.trim().parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    format!("{product_line} {next_n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_line_name_known_and_unknown() {
        assert_eq!(
            product_line_name("929003666501"),
            Some("Hue GU10 Spot CCT/COL")
        );
        assert_eq!(product_line_name("totally-unknown"), None);
    }

    #[test]
    fn cct_line_numbering_ignores_cct_col_names() {
        // "Hue GU10 Spot CCT" is a prefix of "Hue GU10 Spot CCT/COL", so the
        // CCT-only sequence must not be advanced by CCT/COL device names:
        // stripping the shorter prefix leaves "/COL n", which fails the
        // number parse and is ignored.
        let mut existing = HashMap::new();
        existing.insert("dev1".to_string(), "Hue GU10 Spot CCT/COL 7".to_string());
        assert_eq!(
            next_name_in_line(&existing, "Hue GU10 Spot CCT"),
            "Hue GU10 Spot CCT 1"
        );
    }

    #[test]
    fn next_name_in_line_starts_at_one_when_none_taken() {
        let existing = HashMap::new();
        assert_eq!(
            next_name_in_line(&existing, "Hue GU10 Spot CCT/COL"),
            "Hue GU10 Spot CCT/COL 1"
        );
    }

    #[test]
    fn next_name_in_line_continues_the_sequence() {
        let mut existing = HashMap::new();
        existing.insert("dev1".to_string(), "Hue GU10 Spot CCT/COL 1".to_string());
        existing.insert("dev2".to_string(), "Hue GU10 Spot CCT/COL 2".to_string());
        existing.insert("dev3".to_string(), "Hue Filament Globe CCT 1".to_string());
        assert_eq!(
            next_name_in_line(&existing, "Hue GU10 Spot CCT/COL"),
            "Hue GU10 Spot CCT/COL 3"
        );
    }

    #[test]
    fn next_name_in_line_ignores_gaps_and_uses_max_plus_one() {
        // A device named "... 5" (e.g. after manual renumbering) means the
        // next auto-assigned name is 6, not filling the gap at 3/4.
        let mut existing = HashMap::new();
        existing.insert("dev1".to_string(), "Hue GU10 Spot CCT/COL 5".to_string());
        assert_eq!(
            next_name_in_line(&existing, "Hue GU10 Spot CCT/COL"),
            "Hue GU10 Spot CCT/COL 6"
        );
    }
}
