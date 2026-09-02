FROM rust:bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build -p trading-hive --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tzdata && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /src/target/release/trading-hive /usr/local/bin/trading-hive
ENV TZ=America/New_York
ENTRYPOINT ["/usr/local/bin/trading-hive"]
