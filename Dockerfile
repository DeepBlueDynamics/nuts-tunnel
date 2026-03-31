FROM rust:1.85-slim-bookworm AS build-rust
WORKDIR /app
COPY . /app
RUN cargo build --release --package nuts-proxy

FROM python:3.12-slim-bookworm AS build-python
WORKDIR /auth
COPY auth/requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

FROM python:3.12-slim-bookworm
# Copy Rust binary
COPY --from=build-rust /app/target/release/nuts-proxy /usr/local/bin/nuts-proxy
# Copy Python auth service + deps
COPY --from=build-python /usr/local/lib/python3.12/site-packages /usr/local/lib/python3.12/site-packages
COPY --from=build-python /usr/local/bin/uvicorn /usr/local/bin/uvicorn
COPY auth/main.py /auth/main.py

# Install tini for running two processes
RUN apt-get update && apt-get install -y --no-install-recommends tini && rm -rf /var/lib/apt/lists/*

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENV PORT=8080
ENV NUTS_AUTH_URL=http://127.0.0.1:9000
EXPOSE 8080

ENTRYPOINT ["tini", "--"]
CMD ["/entrypoint.sh"]
