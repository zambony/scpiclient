RUSTFLAGS="-Zlocation-detail=none" cargo +nightly build -Z build-std=std,panic_abort --package scpi --bin scpi --release --target x86_64-unknown-linux-gnu && \
RUSTFLAGS="-Zlocation-detail=none" cross +nightly build -Z build-std=std,panic_abort --package scpi --bin scpi --release --target x86_64-pc-windows-msvc