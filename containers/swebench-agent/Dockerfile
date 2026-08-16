ARG BASE_IMAGE
FROM ${BASE_IMAGE}

COPY agent-entrypoint.sh /usr/local/bin/swebench-agent
RUN chmod 0555 /usr/local/bin/swebench-agent

ENTRYPOINT ["/usr/local/bin/swebench-agent"]
