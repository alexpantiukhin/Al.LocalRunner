FROM rust:1.99.1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p lr-cli --bin localrunner \
    && mkdir -p /out/empty \
    && cp target/release/localrunner /out/localrunner

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/localrunner /usr/local/bin/localrunner
COPY --from=build --chown=65532:65532 /out/empty /var/lib/al-localrunner
COPY --from=build --chown=65532:65532 /out/empty /etc/al-localrunner
COPY --from=build --chown=65532:65532 /out/empty /run/al-localrunner
ENV LOCALRUNNER_STATE_DIR=/var/lib/al-localrunner LOCALRUNNER_CONFIG_DIR=/etc/al-localrunner
USER 65532:65532
EXPOSE 7443
ENTRYPOINT []
CMD ["localrunner", "--help"]
