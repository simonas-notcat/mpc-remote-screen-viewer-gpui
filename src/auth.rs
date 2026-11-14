//! Authentication only: the TCP payload is not encrypted or integrity-protected.
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::protocol::{Auth, VIDEO_CODECS};

type HmacSha256 = Hmac<Sha256>;

pub struct Credentials {
    key: Zeroizing<[u8; 32]>,
}
impl Credentials {
    pub fn derive(password: &str) -> Self {
        let mut key = Zeroizing::new([0; 32]);
        pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), b"remote-screen/v1", 10000, &mut *key);
        Self { key }
    }
    fn mac(&self, label: &[u8], server: &[u8; 16], client: &[u8; 16]) -> HmacSha256 {
        let mut mac =
            HmacSha256::new_from_slice(self.key.as_ref()).expect("HMAC accepts any key size");
        mac.update(label);
        mac.update(server);
        mac.update(client);
        mac
    }
    pub fn client_auth(&self, server: &[u8; 16], client: [u8; 16]) -> Auth {
        Auth {
            nonce: client,
            proof: self
                .mac(b"RSCR-client", server, &client)
                .finalize()
                .into_bytes()
                .into(),
            codec_mask: VIDEO_CODECS,
            extension: 1,
        }
    }
    pub fn fresh_auth(&self, server: &[u8; 16]) -> Result<Auth, getrandom::Error> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce)?;
        Ok(self.client_auth(server, nonce))
    }
    /// HMAC's verification operation uses constant-time equality.
    pub fn verify_server(&self, server: &[u8; 16], client: &[u8; 16], proof: &[u8; 32]) -> bool {
        self.mac(b"RSCR-server", server, client)
            .verify_slice(proof)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn derived_key_matches_synthetic_vector() {
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/protocol-v2-vectors.json"))
                .unwrap();
        let v = &vectors["authentication"];
        let credentials = Credentials::derive(v["password"].as_str().unwrap());
        let hex: String = credentials.key.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, v["derived_key_hex"]);
        assert_ne!(
            Credentials::derive(" example-password").key,
            credentials.key
        );
    }
}
