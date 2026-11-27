# syntax=docker/dockerfile:1
# Exact AMD64 manifests verified 2026-11-26; no dependency or source changes.
FROM --platform=linux/amd64 debian:trixie-slim@sha256:ab3b3c8c5159eebfca4376430132d53c4d425f869c765ecbe47177a030da575d AS artifacts
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl gzip tar && rm -rf /var/lib/apt/lists/*
WORKDIR /artifacts
RUN curl --fail --location --connect-timeout 15 --max-time 180 --output cli.tar.gz https://github.com/duckdb/duckdb/releases/download/v2.0.1/duckdb-cli-linux-amd64.tar.gz \
 && echo 'b676268be4d39e4e41a904442c9796c9914e645cc085c7505f048f2c992b5eca  cli.tar.gz' | sha256sum -c - \
 && tar -xzf cli.tar.gz && test -x duckdb && rm cli.tar.gz
# Documented official backup repository; primary returned HTTP 403, error 1010.
RUN curl --fail --location --connect-timeout 15 --max-time 180 --output fts.gz https://extensions.duckdb-backup.org/v2.0.1/linux_amd64/fts.duckdb_extension.gz \
 && echo '50978aea0a8ef2599cefc87fed1cb65920362430a6cad0070d24946103605b3c  fts.gz' | sha256sum -c - \
 && curl --fail --location --connect-timeout 15 --max-time 180 --output vss.gz https://extensions.duckdb-backup.org/v2.0.1/linux_amd64/vss.duckdb_extension.gz \
 && echo '26ef35a7d52f4be1f4c4fdbde84f890b0da896e5318a871e14e546225c478c36  vss.gz' | sha256sum -c - \
 && gzip -d fts.gz vss.gz && mv fts fts.duckdb_extension && mv vss vss.duckdb_extension
RUN --network=none ./duckdb -json -c "SELECT version(); PRAGMA platform; LOAD '/artifacts/fts.duckdb_extension'; LOAD '/artifacts/vss.duckdb_extension'; SELECT extension_name, extension_version, loaded FROM duckdb_extensions() WHERE extension_name IN ('fts','vss');" > signatures.json

FROM --platform=linux/amd64 rust:1.100.0-trixie@sha256:1e626a07e55b67d7fe19de7a590519e0cf4b75cf3788b058fa75cf4d599d9b5a AS builder
RUN rustc --version | grep '^rustc 1.100.0 ' \
 && apt-get update && apt-get install -y --no-install-recommends clang libc++-dev libc++abi-dev cmake pkg-config && rm -rf /var/lib/apt/lists/*
ENV CARGO_BUILD_JOBS=2 NUM_JOBS=2
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
# FastEmbed/ONNX stays enabled exactly as locked. No model inference/download.
RUN cargo build --release --locked -j 2
COPY --from=artifacts /artifacts /artifacts
RUN mkdir -p /runtime/usr/local/bin /runtime/usr/share/agentrecall-poc /runtime/home/agentrecall/.duckdb/extensions/v2.0.1/linux_amd64 \
 && cp target/release/agentrecall /runtime/usr/local/bin/agentrecall \
 && cp /artifacts/duckdb /runtime/usr/local/bin/duckdb \
 && cp /artifacts/*.duckdb_extension /runtime/home/agentrecall/.duckdb/extensions/v2.0.1/linux_amd64/ \
 && cp /artifacts/signatures.json /runtime/usr/share/agentrecall-poc/signatures.json
# Inspect the actual executable/extension library closure, never guess ONNX .so paths.
RUN set -eu; for file in /runtime/usr/local/bin/* /runtime/home/agentrecall/.duckdb/extensions/v2.0.1/linux_amd64/*.duckdb_extension; do \
      echo "ARTIFACT $file"; ldd "$file"; \
    done > /runtime/usr/share/agentrecall-poc/ldd.txt \
 && ! grep -q 'not found' /runtime/usr/share/agentrecall-poc/ldd.txt \
 && awk '/=> \/[^ ]+/ {print $3} /^[[:space:]]*\/[^ ]+/ {print $1}' /runtime/usr/share/agentrecall-poc/ldd.txt | sort -u > /tmp/libs \
 && while read -r lib; do resolved="$(readlink -f "$lib")"; mkdir -p "/runtime$(dirname "$resolved")"; cp -L "$resolved" "/runtime$resolved"; done < /tmp/libs \
 && cp -a /etc/ssl /runtime/etc-ssl \
 && mkdir -p /runtime/etc/ssl && cp -a /etc/ssl/certs /runtime/etc/ssl/ && rm -rf /runtime/etc-ssl \
 && (rustc --version; cargo --version; sha256sum /runtime/usr/local/bin/* /runtime/home/agentrecall/.duckdb/extensions/v2.0.1/linux_amd64/*.duckdb_extension) > /runtime/usr/share/agentrecall-poc/artifacts.txt
# Validate the unchanged Rust CLI against the preprovisioned signed cache, offline.
RUN --network=none HOME=/runtime/home/agentrecall /runtime/usr/local/bin/agentrecall install-fts \
 && HOME=/runtime/home/agentrecall /runtime/usr/local/bin/agentrecall install-vss

FROM --platform=linux/amd64 debian:trixie-slim@sha256:ab3b3c8c5159eebfca4376430132d53c4d425f869c765ecbe47177a030da575d AS runtime
LABEL agentrecall-poc-native-owned="image" agentrecall-poc.module="poc-native-container"
COPY --from=builder /runtime/ /
RUN groupadd --gid 10001 agentrecall && useradd --uid 10001 --gid 10001 --home-dir /home/agentrecall --no-create-home agentrecall \
 && mkdir -p /state/data /state/credentials && chown -R 10001:10001 /state /home/agentrecall \
 && chmod -R a+rX /home/agentrecall/.duckdb /usr/share/agentrecall-poc
ENV HOME=/home/agentrecall
WORKDIR /state
USER 10001:10001
EXPOSE 8789
STOPSIGNAL SIGINT
ENTRYPOINT ["/usr/local/bin/agentrecall"]
CMD ["serve"]
