//! A vendor-side license signing example: reads a license JSON on stdin,
//! prints the `<payload>.<signature>` token on stdout.
//!
//! The signing key stays with the vendor — the framework only ever ships a
//! verifying key. Generate a keypair once with:
//!
//! ```console
//! $ cargo run -p nanobot-auth --example sign-license -- generate
//! ```
//!
//! then keep the seed safe and distribute the verifying key (base64url)
//! with the product. Issue a license:
//!
//! ```console
//! $ echo '{"license_id":"lic-1","plan":"team","seats":5,
//!          "features":["agent.turn"],"expires_at_ms":1798761600000}' \
//!     | cargo run -p nanobot-auth --example sign-license -- sign <seed-hex>
//! ```

use std::io::Read as _;

use ed25519_dalek::SigningKey;
use nanobot_auth::{public_key_to_base64, sign_license, License};

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
                eprintln!("usage: sign-license sign <seed-hex>");
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
            let license: License = serde_json::from_str(&json).expect("license JSON");
            println!("{}", sign_license(&license, &SigningKey::from_bytes(&seed)));
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
