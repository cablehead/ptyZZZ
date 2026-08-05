//! keyprobe: synthetic end-to-end keyboard test target.
//!
//! Run inside a ptyZZZ pane (bench/keytest.nu drives the whole loop). A
//! headless browser replays bench/keyprobe.json's steps as synthesized
//! KeyboardEvents; the client translates them to {t:key}/{t:paste} frames;
//! the emulator encodes them into the pty; this binary sits at the far end
//! in raw mode and asserts the exact bytes arrive, in order.
//!
//! Steps may carry a `pre` byte string the probe writes to the terminal
//! before reading (mode flips: DECCKM, bracketed paste), so mode-dependent
//! encodings are asserted end to end. The driver pauses before those steps
//! so the mode change lands before the key is encoded.
//!
//! Output: one `ok <name>` line per step, then `KEYPROBE PASS <n>` or
//! `KEYPROBE FAIL`; the runner greps the pane's keyframe for the verdict.

use std::io::{Read, Write};
use std::process::Command;

#[derive(serde::Deserialize)]
struct Step {
    name: String,
    #[serde(default)]
    pre: String,
    #[serde(default)]
    expect: String,
}

#[derive(serde::Deserialize)]
struct Spec {
    steps: Vec<Step>,
}

fn stty(arg: &str) {
    let _ = Command::new("stty").arg(arg).status();
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: keyprobe <spec.json>");
    let spec: Spec =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read spec")).expect("parse");

    // raw: byte-at-a-time, no echo, no ISIG (^C must arrive as 0x03)
    stty("raw");
    stty("-echo");
    // A stuck read means a key never arrived; report rather than hang.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(25));
        print!("\r\nKEYPROBE TIMEOUT\r\n");
        stty("sane");
        std::process::exit(2);
    });

    let mut stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut pass = 0usize;
    for step in &spec.steps {
        if !step.pre.is_empty() {
            let _ = out.write_all(step.pre.as_bytes());
            let _ = out.flush();
        }
        if step.expect.is_empty() {
            continue;
        }
        let want = step.expect.as_bytes();
        let mut got = vec![0u8; want.len()];
        if stdin.read_exact(&mut got).is_err() {
            print!("\r\nFAIL {} (stdin closed)\r\nKEYPROBE FAIL\r\n", step.name);
            stty("sane");
            std::process::exit(1);
        }
        if got == want {
            pass += 1;
            print!("\r\nok {}", step.name);
            let _ = out.flush();
        } else {
            // Alignment is lost after a mismatch; bail with the evidence.
            print!(
                "\r\nFAIL {} want {} got {}\r\nKEYPROBE FAIL\r\n",
                step.name,
                hex(want),
                hex(&got)
            );
            stty("sane");
            std::process::exit(1);
        }
    }
    print!("\r\nKEYPROBE PASS {pass}\r\n");
    stty("sane");
}
