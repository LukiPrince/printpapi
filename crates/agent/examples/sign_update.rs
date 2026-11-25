// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The signer of the CI smoke test: `sign_update <payload file> <output file>` writes a signed
//! `printpapi-update.txt` (one `sig` line, an empty line, the payload) made with a fixed test seed.
//!
//! The seed is public on purpose. Its public half is in the agent only when the Cargo feature
//! `update-test-key` is on, which release.yml never does; a real release is signed by
//! `tools/release/update_manifest.py` with the key of the protected GitHub environment.

use ring::signature::Ed25519KeyPair;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [payload_path, out_path] = args.as_slice() else {
        eprintln!("usage: sign_update <payload file> <output file>");
        std::process::exit(2);
    };
    let payload = std::fs::read(payload_path).expect("cannot read the payload file");
    let pair = Ed25519KeyPair::from_seed_unchecked(b"printpapi test seed, not secret!")
        .expect("the seed is 32 bytes");
    let sig: String = pair
        .sign(&payload)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut out = format!("sig {sig}\n\n").into_bytes();
    out.extend_from_slice(&payload);
    std::fs::write(out_path, out).expect("cannot write the output file");
}
