FROM node:lts-slim AS builder
WORKDIR /build
RUN corepack enable
COPY Frontend/package.json Frontend/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY Frontend/ .
RUN pnpm build

FROM node:lts-slim AS runtime
WORKDIR /app
RUN useradd --system --create-home --shell /usr/sbin/nologin tower
COPY --from=builder /build/build ./build
COPY --from=builder /build/package.json ./package.json
COPY --from=builder /build/node_modules ./node_modules
ENV NODE_ENV=production
ENV PORT=3000
USER tower
EXPOSE 3000
CMD ["node", "build"]
