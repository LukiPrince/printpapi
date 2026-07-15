FROM python:3.12-slim
# cups-client: `lp` for the network Bixolon (CUPS+driver renders PDF -> label).
# Talks to the host cupsd via the mounted /run/cups/cups.sock (see compose).
RUN apt-get update \
    && apt-get install -y --no-install-recommends cups-client \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY app/ ./app/
EXPOSE 3460
# ponytail: stdlib only, kein requirements.txt nötig
CMD ["python", "-m", "app.server"]
