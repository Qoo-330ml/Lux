use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

pub const WEB_PLAYBACK_SESSION_TTL_SECONDS: i64 = 15 * 60;

#[derive(Clone)]
pub struct ResourceSigner {
    key: [u8; 32],
}

impl ResourceSigner {
    pub fn random() -> Self {
        let mut key = [0_u8; 32];
        OsRng.fill_bytes(&mut key);
        Self { key }
    }

    pub fn sign(&self, session_id: &str, resource: &str, expires_at: i64) -> Option<String> {
        let mut mac = HmacSha256::new_from_slice(&self.key).ok()?;
        mac.update(signed_message(session_id, resource, expires_at).as_bytes());
        Some(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }

    pub fn verify(
        &self,
        session_id: &str,
        resource: &str,
        expires_at: i64,
        signature: &str,
        now: i64,
    ) -> bool {
        if expires_at < now {
            return false;
        }
        let Some(expected) = self.sign(session_id, resource, expires_at) else {
            return false;
        };
        expected.as_bytes().ct_eq(signature.as_bytes()).into()
    }
}

pub fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

fn signed_message(session_id: &str, resource: &str, expires_at: i64) -> String {
    format!("lux-web-playback\n{session_id}\n{resource}\n{expires_at}")
}

#[cfg(test)]
mod tests {
    use super::{ResourceSigner, unix_timestamp};

    #[test]
    fn signatures_are_bound_to_the_session_resource_and_expiry() {
        let signer = ResourceSigner::random();
        let expires_at = unix_timestamp() + 60;
        let signature = signer.sign("session-1", "direct", expires_at).unwrap();

        assert!(signer.verify(
            "session-1",
            "direct",
            expires_at,
            &signature,
            unix_timestamp()
        ));
        assert!(!signer.verify(
            "session-2",
            "direct",
            expires_at,
            &signature,
            unix_timestamp()
        ));
        assert!(!signer.verify("session-1", "hls", expires_at, &signature, unix_timestamp()));
        assert!(!signer.verify(
            "session-1",
            "direct",
            expires_at - 1,
            &signature,
            unix_timestamp()
        ));
    }

    #[test]
    fn expired_signatures_are_rejected_before_comparison() {
        let signer = ResourceSigner::random();
        let expires_at = unix_timestamp() - 1;
        let signature = signer.sign("session-1", "direct", expires_at).unwrap();

        assert!(!signer.verify(
            "session-1",
            "direct",
            expires_at,
            &signature,
            unix_timestamp()
        ));
    }
}
