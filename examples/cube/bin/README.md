# prebuilt face animations

The cube's animation faces run prebuilt binaries vendored here, one per
target triplet:

    play_style-<triplet>     anima's play_style example
    asciiquarium-<triplet>   asciiquarium-rs

`serve.nu` picks `<name>-<triplet>` for the platform http-nu runs on and
fails at load if it is missing. The triplet names the host, not the build
target, so a musl build is still named `-gnu` on Linux.

To add your platform:

    triplet=$(rustc -vV | sed -n 's/host: //p')

    rustup target add x86_64-unknown-linux-musl
    git clone https://github.com/Yazelix/anima
    (cd anima && cargo build --release \
      --target x86_64-unknown-linux-musl --example play_style)
    cp anima/target/x86_64-unknown-linux-musl/release/examples/play_style \
      play_style-$triplet

    git clone https://github.com/cablehead/asciiquarium-rs
    (cd asciiquarium-rs && cargo build --release)
    cp asciiquarium-rs/target/release/asciiquarium-rs asciiquarium-$triplet

play_style is built against musl so it runs on any glibc. macOS has no musl
target. Drop `--target` there.

Vendored: anima f704fef, formerly yazelix-screen. Built with rustc 1.96.1.
