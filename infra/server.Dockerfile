FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p remote-server

FROM debian:bookworm-slim
RUN useradd -r -u 10001 remote
COPY --from=build /src/target/release/remote-server /usr/local/bin/remote-server
USER remote
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/remote-server"]
