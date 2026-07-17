// Backend/crates/api-gateway/src/branding.rs
//! `Branding` — the `site_settings` row at `key = "price_page"` (yes, historically named after
//! the public price page it originally only served; the reference's own naming, kept verbatim
//! for continuity with the stored data shape, not renamed). Validation ports the reference's
//! `validateBranding`/`isSafeOutboundUrl`-adjacent rules exactly: PNG/JPEG/WEBP-only data URIs
//! (SVG/ICO rejected — SVG can embed executable script if opened as a top-level document), 5MB
//! decoded cap each.
use serde::{Deserialize, Serialize};

pub const SITE_SETTINGS_KEY: &str = "price_page";

const TITLE_MAX: usize = 60;
const SUBTITLE_MAX: usize = 160;
const SITE_NAME_MAX: usize = 60;
const BRAND_TAG_MAX: usize = 20;
const LOGO_MAX_BYTES: usize = 5 * 1024 * 1024;
const FAVICON_MAX_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Branding {
    pub title: String,
    pub subtitle: String,
    pub site_name: String,
    pub brand_tag: String,
    pub logo_data_uri: Option<String>,
    pub favicon_data_uri: Option<String>,
}

impl Default for Branding {
    fn default() -> Self {
        Self {
            title: "Harga Harga".to_string(),
            subtitle: "Daftar harga rute per jenis kendaraan — SPX Portal".to_string(),
            site_name: "SPX Agency Portal".to_string(),
            brand_tag: String::new(),
            logo_data_uri: None,
            favicon_data_uri: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct BrandingInput {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub site_name: String,
    #[serde(default)]
    pub brand_tag: String,
    #[serde(default)]
    pub logo_data_uri: Option<String>,
    #[serde(default)]
    pub favicon_data_uri: Option<String>,
}

/// Validates a `data:image/(png|jpeg|webp);base64,...` URI, rejecting every other image type
/// (SVG/ICO especially) and anything exceeding `max_bytes` DECODED size. Computes decoded length
/// from base64 length + padding (matching the reference's own `decodedSize()` helper) rather than
/// actually decoding — avoids allocating the full image just to check its size.
fn validate_data_uri(value: &str, max_bytes: usize) -> Result<(), String> {
    let prefixes = [
        "data:image/png;base64,",
        "data:image/jpeg;base64,",
        "data:image/webp;base64,",
    ];
    let Some(b64) = prefixes.iter().find_map(|p| value.strip_prefix(p)) else {
        return Err("must be a data:image/(png|jpeg|webp);base64,... URI (svg/ico are not allowed)".to_string());
    };
    if b64.is_empty()
        || !b64
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
    {
        return Err("invalid base64 payload".to_string());
    }
    if b64.len() < 4 {
        return Err("invalid base64 payload".to_string());
    }
    let padding = b64.chars().rev().take_while(|&c| c == '=').count().min(2);
    let decoded_len = (b64.len() / 4) * 3 - padding;
    if decoded_len > max_bytes {
        return Err(format!("image exceeds {max_bytes} bytes decoded"));
    }
    Ok(())
}

pub fn validate_and_normalize(input: BrandingInput) -> Result<Branding, String> {
    let title = input.title.trim();
    if title.is_empty() {
        return Err("title is required".to_string());
    }
    if title.chars().count() > TITLE_MAX {
        return Err(format!("title exceeds {TITLE_MAX} characters"));
    }
    let subtitle = input.subtitle.trim();
    if subtitle.chars().count() > SUBTITLE_MAX {
        return Err(format!("subtitle exceeds {SUBTITLE_MAX} characters"));
    }
    let site_name_trimmed = input.site_name.trim();
    let site_name = if site_name_trimmed.is_empty() {
        Branding::default().site_name
    } else {
        site_name_trimmed.to_string()
    };
    if site_name.chars().count() > SITE_NAME_MAX {
        return Err(format!("site_name exceeds {SITE_NAME_MAX} characters"));
    }
    let brand_tag = input.brand_tag.trim();
    if brand_tag.chars().count() > BRAND_TAG_MAX {
        return Err(format!("brand_tag exceeds {BRAND_TAG_MAX} characters"));
    }
    let logo_data_uri = input.logo_data_uri.filter(|s| !s.is_empty());
    if let Some(logo) = &logo_data_uri {
        validate_data_uri(logo, LOGO_MAX_BYTES)?;
    }
    let favicon_data_uri = input.favicon_data_uri.filter(|s| !s.is_empty());
    if let Some(favicon) = &favicon_data_uri {
        validate_data_uri(favicon, FAVICON_MAX_BYTES)?;
    }
    Ok(Branding {
        title: title.to_string(),
        subtitle: subtitle.to_string(),
        site_name,
        brand_tag: brand_tag.to_string(),
        logo_data_uri,
        favicon_data_uri,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_input() -> BrandingInput {
        BrandingInput {
            title: "My Title".to_string(),
            subtitle: String::new(),
            site_name: String::new(),
            brand_tag: String::new(),
            logo_data_uri: None,
            favicon_data_uri: None,
        }
    }

    #[test]
    fn blank_title_is_rejected() {
        let mut input = base_input();
        input.title = "   ".to_string();
        assert!(validate_and_normalize(input).is_err());
    }

    #[test]
    fn blank_site_name_falls_back_to_default() {
        let branding = validate_and_normalize(base_input()).expect("valid");
        assert_eq!(branding.site_name, Branding::default().site_name);
    }

    #[test]
    fn svg_data_uri_is_rejected() {
        let mut input = base_input();
        input.logo_data_uri = Some("data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=".to_string());
        let err = validate_and_normalize(input).unwrap_err();
        assert!(err.contains("svg/ico are not allowed") || err.contains("must be a data:image"));
    }

    #[test]
    fn oversized_logo_is_rejected() {
        let mut input = base_input();
        // 7,200,000 base64 'A' characters decode to (7_200_000/4)*3 = 5,400,000 bytes — safely
        // over the 5MB (5*1024*1024 = 5,242,880 bytes) cap. NOTE: the brief's own literal here
        // was 6_900_000, which decodes to only 5,175,000 bytes — UNDER the actual binary-MiB cap
        // (its doc comment conflated a decimal-MB "just over 5MB" with the binary-MiB constant
        // the code actually uses) — caught by this test's own RED/GREEN cycle failing GREEN with
        // the brief's original value, so bumped here to genuinely exceed the real cap.
        let huge_b64 = "A".repeat(7_200_000);
        input.logo_data_uri = Some(format!("data:image/png;base64,{huge_b64}"));
        assert!(validate_and_normalize(input).is_err());
    }

    #[test]
    fn valid_png_logo_is_accepted() {
        let mut input = base_input();
        input.logo_data_uri = Some("data:image/png;base64,iVBORw0KGgo=".to_string());
        let branding = validate_and_normalize(input).expect("valid small PNG must pass");
        assert!(branding.logo_data_uri.is_some());
    }

    #[test]
    fn short_padding_heavy_base64_is_rejected_not_panicking() {
        let mut input = base_input();
        // b64 body is "==" — 2 chars, both padding. (b64.len() / 4) * 3 = 0 while padding = 2,
        // which would underflow a plain `0usize - 2` (panics under overflow-checks, silently
        // wraps to usize::MAX in release). Must cleanly return Err, not panic.
        input.logo_data_uri = Some("data:image/png;base64,==".to_string());
        let err = validate_and_normalize(input).unwrap_err();
        assert!(err.contains("invalid base64 payload"));
    }
}
