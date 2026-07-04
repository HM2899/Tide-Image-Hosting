FROM rust:1.96-bookworm AS builder
WORKDIR /app
RUN rustup target add wasm32-unknown-unknown && cargo install trunk --locked
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
COPY migrations ./migrations
RUN cd crates/web && trunk build index.html --release --dist /app/frontend --minify false
RUN cargo build --release -p tide-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libwebp7 libjpeg62-turbo libpng16-16 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/tide-server /usr/local/bin/tide-server
COPY migrations ./migrations
COPY --from=builder /app/frontend ./frontend
ENV APP_ENV=production
EXPOSE 8080
CMD ["tide-server"]
