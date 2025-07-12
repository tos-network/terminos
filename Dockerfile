FROM lukemathwalker/cargo-chef:0.1.71-rust-1.86-slim-bookworm AS chef

ENV BUILD_DIR=/tmp/terminos-build

RUN mkdir -p $BUILD_DIR
WORKDIR $BUILD_DIR

# ---

FROM chef AS planner

ARG app

COPY . .
RUN cargo chef prepare --recipe-path recipe.json --bin $app

# ---

FROM chef AS builder

ARG app
ARG commit_hash

COPY --from=planner /tmp/terminos-build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --bin $app

COPY Cargo.toml Cargo.lock ./
COPY terminos_common ./terminos_common
COPY $app ./$app

RUN TERMINOS_COMMIT_HASH=${commit_hash} cargo build --release --bin $app

# ---

FROM gcr.io/distroless/cc-debian12

ARG app

ENV APP_DIR=/var/run/terminos
ENV DATA_DIR=$APP_DIR/data
ENV BINARY=$APP_DIR/terminos

LABEL org.opencontainers.image.authors="Slixe <slixeprivate@gmail.com>"

COPY --from=builder /tmp/terminos-build/target/release/$app $BINARY

WORKDIR $DATA_DIR

ENTRYPOINT ["/var/run/terminos/terminos"]
