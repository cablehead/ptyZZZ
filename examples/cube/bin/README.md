# prebuilt face animations

The cube's animation faces run prebuilt binaries vendored here, one per
target triplet:

    play_style-<triplet>     yazelix-screen's play_style example
    asciiquarium-<triplet>   asciiquarium-rs

`serve.nu` picks `<name>-<triplet>` for the platform http-nu runs on and
fails at load if it is missing. To add your platform:

    triplet=$(rustc -vV | sed -n 's/host: //p')

    git clone https://github.com/luccahuguet/yazelix-screen
    (cd yazelix-screen && cargo build --release --example play_style)
    cp yazelix-screen/target/release/examples/play_style play_style-$triplet

    git clone https://github.com/cablehead/asciiquarium-rs
    (cd asciiquarium-rs && cargo build --release)
    cp asciiquarium-rs/target/release/asciiquarium-rs asciiquarium-$triplet
