//! WCAG 2.x color parsing, relative luminance, and contrast ratio math.

/// Parses a strict `#RRGGBB` 6-digit ASCII hex color into `(r, g, b)` channels.
#[must_use]
pub fn parse_hex_color(hex: &str) -> Option<(u8, u8, u8)> {
    let bytes = hex.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' || !bytes[1..].iter().all(u8::is_ascii_hexdigit) {
        return None;
    }

    let r = u8::from_str_radix(&hex[1..3], 16).ok()?;
    let g = u8::from_str_radix(&hex[3..5], 16).ok()?;
    let b = u8::from_str_radix(&hex[5..7], 16).ok()?;
    Some((r, g, b))
}

/// Returns `true` if `hex` is a strict `#RRGGBB` 6-digit hex color string.
#[must_use]
pub fn is_valid_hex_color(hex: &str) -> bool {
    parse_hex_color(hex).is_some()
}

fn linearize_srgb(channel: u8) -> f64 {
    let x = f64::from(channel) / 255.0;
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

/// Computes WCAG 2.x relative luminance in `[0.0, 1.0]` for an sRGB triplet.
#[must_use]
pub fn relative_luminance(r: u8, g: u8, b: u8) -> f64 {
    0.2126 * linearize_srgb(r) + 0.7152 * linearize_srgb(g) + 0.0722 * linearize_srgb(b)
}

/// Computes the WCAG 2.x contrast ratio between two `#RRGGBB` colors.
///
/// Returns `None` if either color string is not a valid `#RRGGBB` hex color.
#[must_use]
pub fn contrast_ratio(fg_hex: &str, bg_hex: &str) -> Option<f64> {
    let (fr, fg, fb) = parse_hex_color(fg_hex)?;
    let (br, bg, bb) = parse_hex_color(bg_hex)?;
    let l1 = relative_luminance(fr, fg, fb);
    let l2 = relative_luminance(br, bg, bb);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    Some((hi + 0.05) / (lo + 0.05))
}

/// Returns `true` if the pair meets WCAG 2.x AA contrast for normal text (`>= 4.5`).
#[must_use]
pub fn meets_wcag_aa_normal(fg_hex: &str, bg_hex: &str) -> bool {
    contrast_ratio(fg_hex, bg_hex).is_some_and(|ratio| ratio >= 4.5)
}

/// Returns `true` if the pair meets WCAG 2.x AA contrast for large text and UI components (`>= 3.0`).
#[must_use]
pub fn meets_wcag_aa_large(fg_hex: &str, bg_hex: &str) -> bool {
    contrast_ratio(fg_hex, bg_hex).is_some_and(|ratio| ratio >= 3.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round2(val: f64) -> f64 {
        (val * 100.0).round() / 100.0
    }

    #[test]
    fn parses_strict_hex_colors() {
        assert_eq!(parse_hex_color("#241207"), Some((0x24, 0x12, 0x07)));
        assert_eq!(parse_hex_color("#e9e2d2"), Some((0xE9, 0xE2, 0xD2)));
        assert_eq!(parse_hex_color("#000000"), Some((0, 0, 0)));
        assert_eq!(parse_hex_color("#FFFFFF"), Some((255, 255, 255)));

        for bad in [
            "",
            "#",
            "#fff",
            "#FFFFFFF",
            "241207",
            "#GGGGGG",
            "#+10203",
            "#241207 ",
            " #241207",
        ] {
            assert_eq!(parse_hex_color(bad), None, "expected None for {bad:?}");
            assert!(!is_valid_hex_color(bad));
        }
        assert!(is_valid_hex_color("#241207"));
    }

    #[test]
    fn computes_exact_wcag_contrast_ratios() {
        let black_white = contrast_ratio("#000000", "#FFFFFF").expect("valid hex");
        assert_eq!(round2(black_white), 21.0);
        assert_eq!(round2(relative_luminance(0, 0, 0)), 0.0);
        assert_eq!(round2(relative_luminance(255, 255, 255)), 1.0);

        let dark_on_cream = contrast_ratio("#241207", "#E9E2D2").expect("valid hex");
        assert_eq!(round2(dark_on_cream), 13.97);
        assert!(meets_wcag_aa_normal("#241207", "#E9E2D2"));
        assert!(meets_wcag_aa_large("#241207", "#E9E2D2"));

        let light_on_burgundy = contrast_ratio("#F6F1E7", "#7A1F2B").expect("valid hex");
        assert_eq!(round2(light_on_burgundy), 9.06);
        assert!(meets_wcag_aa_normal("#F6F1E7", "#7A1F2B"));
        assert!(meets_wcag_aa_large("#F6F1E7", "#7A1F2B"));

        // Symmetric regardless of fg/bg order
        let reversed = contrast_ratio("#7A1F2B", "#F6F1E7").expect("valid hex");
        assert!((light_on_burgundy - reversed).abs() < 1e-12);
    }

    #[test]
    fn rejects_failing_and_invalid_pairs() {
        // Low contrast pairs
        assert!(!meets_wcag_aa_normal("#D4B59E", "#FAF8F5"));
        assert!(!meets_wcag_aa_large("#D4B59E", "#FAF8F5"));
        assert!(!meets_wcag_aa_normal("#E8E1D1", "#FFFFFF"));
        assert!(!meets_wcag_aa_normal("#333333", "#1C1917"));

        // Passes large text (>= 3.0) but fails normal text (< 4.5): ~3.10
        let border_ratio = contrast_ratio("#949494", "#FFFFFF").expect("valid hex");
        assert!(border_ratio >= 3.0 && border_ratio < 4.5);
        assert!(meets_wcag_aa_large("#949494", "#FFFFFF"));
        assert!(!meets_wcag_aa_normal("#949494", "#FFFFFF"));

        // Invalid hex
        assert_eq!(contrast_ratio("#BAD", "#FFFFFF"), None);
        assert!(!meets_wcag_aa_normal("#BAD", "#FFFFFF"));
        assert!(!meets_wcag_aa_large("#BAD", "#FFFFFF"));
    }
}
