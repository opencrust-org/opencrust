# Stage 1: Build
FROM rust:1.93-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Clone from GitHub
RUN apt-get update && apt-get install -y git && rm -rf /var/lib/apt/lists/*
RUN git clone https://github.com/opencrust-org/opencrust.git .

# Build release binary
RUN cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/opencrust /usr/local/bin/opencrust

RUN useradd -m -s /bin/bash opencrust
USER opencrust
WORKDIR /home/opencrust

EXPOSE 3000

ENTRYPOINT ["opencrust"]
CMD ["start", "--host", "0.0.0.0"]
