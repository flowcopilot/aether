# Bob: OpenClaw, the aether OpenClaw plugin and the aether binary of this
# checkout in one image. Build context: integrations/openclaw-plugin. The
# compose file adds the `aether` image and the `demo` folder as contexts.
FROM node:26-trixie-slim AS plugin
WORKDIR /plugin
COPY package.json package-lock.json tsconfig.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund
COPY index.ts ./
COPY src ./src
RUN npm run build

FROM node:26-trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates openssl socat \
 && rm -rf /var/lib/apt/lists/*
RUN npm install -g --no-audit --no-fund openclaw@2026.10.10
COPY --from=aether /usr/local/bin/aether /usr/local/bin/aether
COPY package.json openclaw.plugin.json README.md /opt/aether-plugin/
COPY --from=plugin /plugin/dist /opt/aether-plugin/dist
COPY --from=demo openclaw.json run.sh /opt/demo/
# Install the plugin first: the config names the aether channel, which is
# unknown until the plugin is installed.
RUN openclaw plugins install --link /opt/aether-plugin --force --accept-capabilities \
 && cp /opt/demo/openclaw.json /root/.openclaw/openclaw.json \
 && openclaw plugins enable aether --accept-capabilities \
 && openclaw config validate \
 && mkdir -p /root/.aether
ENTRYPOINT ["sh", "/opt/demo/run.sh"]
