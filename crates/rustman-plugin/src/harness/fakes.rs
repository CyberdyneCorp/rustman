//! The fake services behind the host imports in a plugin test: a fixed
//! clock, seeded random bytes, a fixed environment and canned HTTP
//! responses. Grants are still checked by the real sandbox first; these
//! only replace what a granted call reaches.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use crate::harness::cases::{FakeResponse, Fakes};
use crate::plugin::{Header, HostError};
use crate::sandbox::{HostServices, HttpRequest, HttpResponse};

/// Deterministic services built from a case's [`Fakes`].
pub struct FakeServices {
    now: SystemTime,
    state: Mutex<u64>,
    env: BTreeMap<String, String>,
    http: Vec<FakeResponse>,
}

impl FakeServices {
    pub fn new(fakes: &Fakes) -> Result<Self, String> {
        Ok(Self {
            now: fakes.now()?,
            state: Mutex::new(fakes.random_seed.unwrap_or(0)),
            env: fakes.env.clone(),
            http: fakes.http.clone(),
        })
    }
}

impl HostServices for FakeServices {
    fn now(&self) -> SystemTime {
        self.now
    }

    /// SplitMix64 from the seed: the same bytes on every run and platform.
    fn fill_random(&self, bytes: &mut [u8]) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        for chunk in bytes.chunks_mut(8) {
            *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
        }
    }

    fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    fn fetch(&self, _plugin: &str, request: HttpRequest) -> Result<HttpResponse, HostError> {
        let canned = self
            .http
            .iter()
            .find(|r| r.method.eq_ignore_ascii_case(&request.method) && r.url == request.url)
            .ok_or_else(|| {
                HostError::Failed(format!(
                    "no fake response for {} {}",
                    request.method, request.url
                ))
            })?;
        Ok(HttpResponse {
            status: canned.status,
            headers: canned
                .headers
                .iter()
                .map(|(name, value)| Header {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
            body: canned.body.clone().into_bytes(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn services(text: &str) -> FakeServices {
        let fakes: Fakes = toml::from_str(text).unwrap();
        FakeServices::new(&fakes).unwrap()
    }

    #[test]
    fn random_bytes_follow_the_seed() {
        let draw = |seed: u64| {
            let services = services(&format!("random_seed = {seed}"));
            let mut bytes = [0u8; 12];
            services.fill_random(&mut bytes);
            bytes
        };
        assert_eq!(draw(7), draw(7));
        assert_ne!(draw(7), draw(8));
        // SplitMix64's first output for seed 0.
        assert_eq!(draw(0)[..8], 0xe220_a839_7b1d_cdafu64.to_le_bytes());
    }

    #[test]
    fn only_canned_responses_are_served() {
        let services = services(
            "env = { A = \"1\" }\n[[http]]\nmethod = \"post\"\nurl = \"https://auth.example.com/token\"\nstatus = 201\nheaders = { \"content-type\" = \"text/plain\" }\nbody = \"t\"\n",
        );
        let request = |method: &str| HttpRequest {
            method: method.to_owned(),
            url: "https://auth.example.com/token".to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let response = services.fetch("p", request("POST")).unwrap();
        assert_eq!(response.status, 201);
        assert_eq!(response.body, b"t");
        assert_eq!(response.headers[0].name, "content-type");
        let missing = services.fetch("p", request("GET")).unwrap_err();
        assert_eq!(
            missing,
            HostError::Failed("no fake response for GET https://auth.example.com/token".into())
        );
        assert_eq!(services.env("A").as_deref(), Some("1"));
        assert_eq!(services.env("HOME"), None);
    }
}
