# Alice: Hermes Agent, the aether Hermes plugin and the aether binary of this
# checkout in one image. Build context: integrations/hermes-plugin. The
# compose file adds the `aether` image and the `demo` folder as contexts.
FROM nousresearch/hermes-agent:latest

USER root
RUN apt-get -o Acquire::Retries=3 update \
 && apt-get -o Acquire::Retries=3 install -y --no-install-recommends socat \
 && rm -rf /var/lib/apt/lists/*
COPY --from=aether /usr/local/bin/aether /usr/local/bin/aether
COPY --exclude=tests --exclude=__pycache__ . /opt/aether-plugin/
COPY --from=demo run.sh /opt/demo/
# The node's data directory is a volume. Make it here so the volume belongs
# to the hermes user. /opt/data is a VOLUME of the base image, so build
# changes there are lost.
RUN install -d -o hermes -g hermes -m 700 /var/lib/aether

USER hermes
ENV HOME=/opt/data AETHER_DATA_DIR=/var/lib/aether/host
WORKDIR /opt/data
ENTRYPOINT ["sh", "/opt/demo/run.sh"]
