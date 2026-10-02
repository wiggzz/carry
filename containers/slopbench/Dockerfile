ARG BASE_IMAGE
FROM ${BASE_IMAGE}
RUN sed -i 's|http://|https://|g' /etc/apt/sources.list.d/*.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends python3 python3-venv ripgrep ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && python3 -m venv /opt/slop-venv
COPY requirements.txt /opt/slop-requirements.txt
RUN /opt/slop-venv/bin/pip install --no-cache-dir -r /opt/slop-requirements.txt \
    && /opt/slop-venv/bin/pip check
ENV PATH=/opt/slop-venv/bin:/opt/swebench-harness/bin:${PATH}
# No benchmark assets, tests, gold, source checkout, or Git objects in layers.
# The native adapter + current-source Carry binary are mounted read-only at run.
ENTRYPOINT ["/opt/swebench-harness/bin/adapter"]
