FROM elixir:1.17-otp-27-slim AS builder
ENV MIX_ENV=prod

# Build gateway
WORKDIR /app/gateway
COPY bench/elixir/gateway/mix.exs ./
RUN mix local.hex --force && mix local.rebar --force && mix deps.get && mix deps.compile
COPY bench/elixir/gateway/lib/ ./lib/
RUN mix compile

# Build compute
WORKDIR /app/compute
COPY bench/elixir/compute/mix.exs ./
RUN mix local.hex --force && mix local.rebar --force && mix deps.get && mix deps.compile
COPY bench/elixir/compute/lib/ ./lib/
RUN mix compile

# Build store
WORKDIR /app/store
COPY bench/elixir/store/mix.exs ./
RUN mix local.hex --force && mix local.rebar --force && mix deps.get && mix deps.compile
COPY bench/elixir/store/lib/ ./lib/
RUN mix compile

FROM elixir:1.17-otp-27-slim
RUN apt-get update && apt-get install -y --no-install-recommends curl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/gateway /opt/gateway
COPY --from=builder /app/compute /opt/compute
COPY --from=builder /app/store /opt/store

# Default to gateway
CMD ["sh", "-c", "cd /opt/gateway && mix run --no-halt"]
