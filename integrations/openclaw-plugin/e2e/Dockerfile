# OpenClaw, the aether plugin and the aether binary in one image, for
# scripts/openclaw-e2e.sh. Build aether-e2e:local first (Dockerfile.e2e).
# Build context: integrations/openclaw-plugin.
ARG AETHER_IMAGE=aether-e2e:local
FROM ${AETHER_IMAGE} AS aether

FROM node:26-trixie-slim AS plugin
WORKDIR /plugin
COPY package.json package-lock.json tsconfig.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund
COPY index.ts ./
COPY src ./src
RUN npm run build

FROM node:26-trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates openssl \
 && rm -rf /var/lib/apt/lists/*
RUN npm install -g --no-audit --no-fund openclaw@2026.10.9
COPY --from=aether /usr/local/bin/aether /usr/local/bin/aether
COPY package.json openclaw.plugin.json README.md /opt/aether-plugin/
COPY --from=plugin /plugin/dist /opt/aether-plugin/dist
COPY e2e/mock-model.mjs e2e/run-host.sh e2e/openclaw.json /opt/e2e/
# Install the plugin first: the config names the aether channel, which is
# unknown until the plugin is installed.
RUN openclaw plugins install --link /opt/aether-plugin --force --accept-capabilities \
 && cp /opt/e2e/openclaw.json /root/.openclaw/openclaw.json \
 && openclaw plugins enable aether --accept-capabilities \
 && openclaw config validate
ENTRYPOINT ["sh", "/opt/e2e/run-host.sh"]
