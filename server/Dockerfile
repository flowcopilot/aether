# Build stage: compile a release.
FROM hexpm/elixir:1.20.5-erlang-29.1.2-debian-trixie-20261027-slim AS build

RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential ca-certificates openssl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
ENV MIX_ENV=prod

RUN mix local.hex --force && mix local.rebar --force

COPY mix.exs mix.lock ./
RUN mix deps.get --only prod && mix deps.compile

COPY config config
COPY lib lib
COPY priv priv
RUN mix compile --warnings-as-errors && mix release

# Run stage: the release only, as a non-root user.
FROM debian:trixie-20260824-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends libstdc++6 openssl libncurses6 locales ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && sed -i '/en_US.UTF-8/s/^# //' /etc/locale.gen && locale-gen

ENV LANG=en_US.UTF-8 \
    LC_ALL=en_US.UTF-8 \
    PORT=4000 \
    DATABASE_PATH=/data/aether_hub.db

RUN useradd --system --uid 10001 --home /app aether \
 && mkdir -p /data && chown aether /data

WORKDIR /app
COPY --from=build --chown=aether /app/_build/prod/rel/aether_hub ./

USER aether
VOLUME /data
EXPOSE 4000

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD curl -fsS "http://127.0.0.1:${PORT}/healthz" || exit 1

CMD ["/app/bin/aether_hub", "start"]
