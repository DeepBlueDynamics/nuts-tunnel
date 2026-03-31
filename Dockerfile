FROM rust:1.85-slim-bookworm AS build
WORKDIR /app
COPY . /app
RUN cargo build --release --package nuts-proxy

FROM gcr.io/distroless/cc-debian12
COPY --from=build /app/target/release/nuts-proxy /nuts-proxy
ENV PORT=8080
EXPOSE 8080
ENTRYPOINT ["/nuts-proxy"]
