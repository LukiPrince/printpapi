# 1) Build the dashboard. Next.js static export -> plain HTML/JS, so no Node in the
#    runtime image. app/web is .dockerignore'd, so this stage is the only source of it.
# --platform=$BUILDPLATFORM: the export is architecture-independent, so the multi-arch release
# must not re-run this build under QEMU for every target.
FROM --platform=$BUILDPLATFORM node:22-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

# 2) The server itself: stdlib only.
FROM python:3.12-slim
# cups-client: `lp` for the network Bixolon (CUPS+driver renders PDF -> label).
# Talks to the host cupsd via the mounted /run/cups/cups.sock (see compose).
RUN apt-get update \
    && apt-get install -y --no-install-recommends cups-client \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY app/ ./app/
COPY --from=web /web/out ./app/web
EXPOSE 3460
# ponytail: stdlib only, kein requirements.txt nötig
CMD ["python", "-m", "app.server"]
