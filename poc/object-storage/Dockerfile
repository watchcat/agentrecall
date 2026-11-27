# Lightweight exact-artifact image: no Cargo or native source compilation.
# Context: target/cloud-poc/object-storage-artifact-stage, never repository root.
FROM --platform=linux/amd64 debian:bookworm-slim@sha256:311f7a192fd9a92b165a86b32fa3aae9005b11849bfb1db2d4fd784559ee464f
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libstdc++6 libgcc-s1 && rm -rf /var/lib/apt/lists/*
COPY duckdb /usr/local/bin/duckdb
RUN mkdir -p /home/poc /tmp/poc && chown -R 10001:10001 /home/poc /tmp/poc && chmod 700 /home/poc /tmp/poc
ENV HOME=/home/poc
USER 10001:10001
WORKDIR /tmp/poc
# The repository install records normal community trust metadata. A direct LOAD
# of downloaded bytes is not an equivalent installation in DuckDB 2.0.
RUN test "$(duckdb -csv -noheader :memory: 'SELECT version();')" = 'v2.0.1' && duckdb -batch -bail :memory: "INSTALL duckdb_object_storage FROM community; LOAD duckdb_object_storage; INSTALL cache_httpfs FROM community; LOAD cache_httpfs;"
ENTRYPOINT ["/usr/local/bin/duckdb"]
