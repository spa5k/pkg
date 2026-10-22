//! Signed Homebrew API files.
//!
//! Homebrew publishes each API file as a JSON Web Signature with an
//! unencoded payload (RFC 7797): `{"payload": "<JSON text>", "signatures":
//! [...]}`. `brew` accepts a file only with a PS512 signature by its
//! `homebrew-1` key over `protected || "." || payload`. This module applies
//! the same check with the key from the pinned `brew` revision.

use base64::Engine as _;
use serde::Deserialize;

/// `Library/Homebrew/api/homebrew-1.pem` at [`crate::HOMEBREW_PIN`].
const HOMEBREW_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIICIjANBgkqhkiG9w0BAQEFAAOCAg8AMIICCgKCAgEAyKoOYzp1rhwXISRi61BY
XBEr2PalSK8lEVOL2USy7mpy0OubOlFyujawyQcBcCn+uPOJ/WaK+POhNWcLLoiK
L2m8GViaQm7SMwdLKUXFgKSPHcG/1m6Vu+TNBKTfQqT60PjEYIrn5NW9ZrM0cUhK
REmsbeAMBevdSaW9UwY9iIhprrgovvT8SzKhF8ZOIZKXfJX4VNk0y/7VJYNuGGqH
3npxV7OKd4yTGRGqFcC9kJ84me3thiu0yqlOjASmfWIwIwcfp4j6BEM2LuqKd7yX
h51/O+MTthkuxV36moDKfdgdOFsvlCFkziaYLScCX9lOlmZHtOfJTAOXxTmM7qGr
wTGK0vhvTi8k9dBmH/dccredQBtPOfM/FEdeyakGLoTcDguiBS/4El3I2KtF6B2h
OGoBumR915/cI4drr5yPMduZ7gjs7ZEZnVkeVzic24TfUHpnOYzrhucNJtHMBDj9
6d1Gk82AhtuF9KlusLmCb6qXCWQSp/A4RZpN37E/p9q8rLp/7B/zp8X2TVvecPNy
BdMagdktdEqK7WPlYMcUp56JaOph8vqYoU+oGyCpWoLvcXFb75o4eefuu6Rs5SyM
c9JCCJ0DDFPjCRFnGPkvsKxFCzMFqH1jpWH0RQIrgmNVM5PO84iRH9YJsSPQzpMj
KvK/ZH4YgR9wNkBNagFo7lsCAwEAAQ==
-----END PUBLIC KEY-----";

/// The signed API file for `name`, such as `cask` or `formula`.
#[must_use]
pub fn url(name: &str) -> String {
    format!("https://formulae.brew.sh/api/{name}.jws.json")
}

/// Return the payload of a signed Homebrew API file.
///
/// # Errors
///
/// Returns an error when the file is malformed or its `homebrew-1`
/// signature does not verify with Homebrew's key.
pub fn verify(document: &[u8]) -> Result<Vec<u8>, String> {
    verify_with(document, HOMEBREW_KEY)
}

fn verify_with(document: &[u8], pem: &str) -> Result<Vec<u8>, String> {
    #[derive(Deserialize)]
    struct Document {
        payload: String,
        signatures: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        protected: String,
        #[serde(default)]
        header: Header,
        signature: String,
    }
    #[derive(Default, Deserialize)]
    struct Header {
        kid: Option<String>,
    }
    #[derive(Deserialize)]
    struct Protected {
        alg: String,
        b64: Option<bool>,
    }
    // `brew` decodes with Ruby's `urlsafe_decode64`, which accepts both forms.
    let base64url = base64::engine::GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    let document: Document = serde_json::from_slice(document)
        .map_err(|e| format!("the signed API file is malformed: {e}"))?;
    let signature = document
        .signatures
        .iter()
        .find(|signature| signature.header.kid.as_deref() == Some("homebrew-1"))
        .ok_or("the signed API file has no homebrew-1 signature")?;
    let protected: Protected = base64url
        .decode(&signature.protected)
        .ok()
        .and_then(|header| serde_json::from_slice(&header).ok())
        .ok_or("the API signature header is malformed")?;
    // A missing `b64` means an encoded payload, which `brew` refuses too.
    if protected.alg != "PS512" || protected.b64 != Some(false) {
        return Err("the API signature is not PS512 over an unencoded payload".to_string());
    }
    let key = public_key_der(pem)?;
    let value = base64url
        .decode(&signature.signature)
        .map_err(|_| "the API signature is not base64url")?;
    let input = format!("{}.{}", signature.protected, document.payload);
    aws_lc_rs::signature::UnparsedPublicKey::new(
        &aws_lc_rs::signature::RSA_PSS_2048_8192_SHA512,
        key,
    )
    .verify(input.as_bytes(), &value)
    .map_err(|_| "the API signature does not verify with Homebrew's key".to_string())?;
    Ok(document.payload.into_bytes())
}

/// The DER bytes of one PEM `PUBLIC KEY` block.
fn public_key_der(pem: &str) -> Result<Vec<u8>, String> {
    let body = pem
        .trim()
        .strip_prefix("-----BEGIN PUBLIC KEY-----")
        .and_then(|rest| rest.strip_suffix("-----END PUBLIC KEY-----"))
        .ok_or("the Homebrew key is not a PEM public key")?;
    base64::engine::general_purpose::STANDARD
        .decode(body.split_whitespace().collect::<String>())
        .map_err(|_| "the Homebrew key is not base64".to_string())
}

/// The one `tap_git_head` that every record of an API payload names.
///
/// # Errors
///
/// Returns an error when the payload is not a nonempty JSON array whose
/// records all name the same exact 40-hex revision.
pub fn revision(payload: &[u8]) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Record {
        tap_git_head: Option<String>,
    }
    let records: Vec<Record> = serde_json::from_slice(payload)
        .map_err(|e| format!("the API payload is not a JSON array of records: {e}"))?;
    let mut heads = records.into_iter().map(|record| record.tap_git_head);
    match heads.next() {
        Some(Some(first))
            if crate::valid_revision(&first) && heads.all(|head| head.as_ref() == Some(&first)) =>
        {
            Ok(first)
        }
        _ => Err("the API payload does not name one exact tap revision".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::encoding::AsDer as _;
    use aws_lc_rs::rand::SystemRandom;
    use aws_lc_rs::signature::{KeyPair as _, RSA_PSS_SHA512, RsaKeyPair};

    const PROTECTED: &str = "eyJhbGciOiJQUzUxMiIsImI2NCI6ZmFsc2UsImNyaXQiOlsiYjY0Il19";

    struct Signer {
        key: RsaKeyPair,
        pem: String,
    }

    impl Signer {
        fn new() -> Self {
            let key = RsaKeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
            let der = key.public_key().as_der().unwrap();
            let body = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
            let pem = format!("-----BEGIN PUBLIC KEY-----\n{body}\n-----END PUBLIC KEY-----\n");
            Self { key, pem }
        }

        fn document(&self, protected: &str, payload: &str, kid: &str) -> Vec<u8> {
            let mut value = vec![0; self.key.public_modulus_len()];
            self.key
                .sign(
                    &RSA_PSS_SHA512,
                    &SystemRandom::new(),
                    format!("{protected}.{payload}").as_bytes(),
                    &mut value,
                )
                .unwrap();
            serde_json::to_vec(&serde_json::json!({
                "payload": payload,
                "signatures": [{
                    "protected": protected,
                    "header": {"kid": kid},
                    "signature": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value),
                }],
            }))
            .unwrap()
        }
    }

    #[test]
    fn accepts_only_a_ps512_homebrew_signature_over_the_exact_payload() {
        let signer = Signer::new();
        let payload =
            r#"[{"token":"a","tap_git_head":"0123456789abcdef0123456789abcdef01234567"}]"#;
        let document = signer.document(PROTECTED, payload, "homebrew-1");
        assert_eq!(
            verify_with(&document, &signer.pem).unwrap(),
            payload.as_bytes()
        );
        // Another key, another key id, or a changed payload fails.
        assert!(verify_with(&document, &Signer::new().pem).is_err());
        assert!(verify(&document).is_err());
        let other = signer.document(PROTECTED, payload, "homebrew-2");
        assert!(verify_with(&other, &signer.pem).is_err());
        let text = String::from_utf8(document).unwrap();
        let changed = text.replace(r#"\"a\""#, r#"\"b\""#);
        assert_ne!(changed, text);
        assert!(verify_with(changed.as_bytes(), &signer.pem).is_err());
        // An encoded payload or another algorithm fails, signed or not.
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"PS512"}"#);
        assert!(
            verify_with(
                &signer.document(&encoded, payload, "homebrew-1"),
                &signer.pem
            )
            .is_err()
        );
        let rs512 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"RS512","b64":false,"crit":["b64"]}"#);
        assert!(verify_with(&signer.document(&rs512, payload, "homebrew-1"), &signer.pem).is_err());
    }

    #[test]
    fn the_pinned_homebrew_key_is_a_4096_bit_public_key() {
        let der = public_key_der(HOMEBREW_KEY).unwrap();
        assert!(der.len() > 512, "{}", der.len());
    }

    #[test]
    fn a_payload_names_exactly_one_revision() {
        let head = "0123456789abcdef0123456789abcdef01234567";
        let one = format!(r#"[{{"tap_git_head":"{head}"}},{{"tap_git_head":"{head}"}}]"#);
        assert_eq!(revision(one.as_bytes()).unwrap(), head);
        let two = format!(
            r#"[{{"tap_git_head":"{head}"}},{{"tap_git_head":"1123456789abcdef0123456789abcdef01234567"}}]"#
        );
        for bad in [
            two.as_str(),
            "[]",
            r#"[{"tap_git_head":"main"}]"#,
            r"[{}]",
            "{}",
        ] {
            assert!(revision(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
