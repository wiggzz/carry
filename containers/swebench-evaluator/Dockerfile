ARG BASE_IMAGE
FROM ${BASE_IMAGE}

COPY evaluator-entrypoint.sh /usr/local/bin/swebench-evaluator
RUN chmod 0555 /usr/local/bin/swebench-evaluator

ENTRYPOINT ["/usr/local/bin/swebench-evaluator"]
