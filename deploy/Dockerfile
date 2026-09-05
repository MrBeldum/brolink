FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p brolink-relay

FROM debian:bookworm-slim
COPY --from=build /src/target/release/brolink-relay /usr/local/bin/brolink-relay
EXPOSE 47851/udp
USER nobody
ENV RUST_LOG=info
ENTRYPOINT ["brolink-relay", "--bind", "0.0.0.0:47851"]
