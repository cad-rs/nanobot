//! A vendor-side plugin license signing example: reads the claims JSON on
//! stdin, prints the `<payload>.<signature>` token for `license.lic`.
//!
//! ```console
//! $ cargo run -p nanobot-auth --example sign-plugin -- generate
//! $ echo '{"plugin_id":"acme.chart","vendor":"acme","features":["panel"],"expires_at_ms":1798761600000}' \
//!     | cargo run -p nanobot-auth --example sign-plugin -- sign <seed-hex>
//! ```
//!
//! The signing key stays with the vendor. Ship the verifying key with the
//! product and register it for the plugin id (or `"*"`); a plugin whose
//! `license.lic` does not verify is rejected by the plugin host.

use std::io::Read as _;

use ed25519_dalek::SigningKey;
use nanobot_auth::{public_key_to_base64, sign_plugin_license, PluginClaims};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "sign".into());
    match mode.as_str() {
        "generate" => {
            let mut seed = [0_u8; 32];
            getrandom::getrandom(&mut seed).expect("os entropy");
            let key = SigningKey::from_bytes(&seed);
            println!("signing seed (hex, KEEP SECRET): {}", hex(&seed));
            println!(
                "verifying key (base64url, ship with the product): {}",
                public_key_to_base64(&key.verifying_key())
            );
        }
        "sign" => {
            let Some(seed_hex) = std::env::args().nth(2) else {
                eprintln!("usage: sign-plugin sign <seed-hex>");
                std::process::exit(2);
            };
            let seed_bytes = decode_hex(&seed_hex);
            let mut seed = [0_u8; 32];
            if seed_bytes.len() != 32 {
                eprintln!("the seed must be 64 hex characters");
                std::process::exit(2);
            }
            seed.copy_from_slice(&seed_bytes);
            let mut json = String::new();
            std::io::stdin().read_to_string(&mut json).expect("read stdin");
            let claims: PluginClaims = serde_json::from_str(&json).expect("claims JSON");
            println!("{}", sign_plugin_license(&claims, &SigningKey::from_bytes(&seed)));
        }
        other => {
            eprintln!("unknown mode `{other}`; use `generate` or `sign <seed-hex>`");
            std::process::exit(2);
        }
    }
}

fn decode_hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex byte"))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
