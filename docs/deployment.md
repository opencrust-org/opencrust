# Docker Deployment

This directory contains examples for deploying OpenCrust using Docker Compose.

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) installed.
- [Docker Compose](https://docs.docker.com/compose/install/) (included with Docker Desktop/Plugin).

## Setup

1. Copy `.env.example` to `.env` and fill in your API keys:
   ```bash
   cp .env.example .env
   # Edit .env with your favorite editor
   ```

## Example 1: Local Gateway (Cloud Providers)

This configuration runs OpenCrust connected to cloud LLM providers (Anthropic, OpenAI, etc.). It uses `docs/deployment/config.basic.yml`.

```bash
docker compose -f docker-compose.yml up --build -d
```

OpenCrust will be available at `http://localhost:3888`.

## Example 2: Gateway + Local Ollama

This configuration runs OpenCrust alongside a local Ollama instance in the same Docker network. It uses `docs/deployment/config.ollama.yml`.

1. Start the services:
   ```bash
   docker compose -f docker-compose.ollama.yml up --build -d
   ```

2. **Important:** You must pull the LLM model inside the Ollama container before OpenCrust can use it.
   ```bash
   docker compose -f docker-compose.ollama.yml exec ollama ollama pull llama3.1
   ```
   *(Note: Adjust `llama3.1` if you changed the model in `docs/deployment/config.ollama.yml`)*

3. OpenCrust will be available at `http://localhost:3888` and will communicate with Ollama internally at `http://ollama:11434`.

## Configuration

The examples use configuration files located in `docs/deployment/`.
- `config.basic.yml`: Default configuration for cloud providers.
- `config.ollama.yml`: Configuration pointing to the internal Ollama service.

These files are mounted to `/home/opencrust/.config/opencrust/config.yml` inside the container. To customize the configuration, you can edit these files or create your own and update the `docker-compose.yml` volume mapping.

## Troubleshooting

### "Connection refused" to Ollama
Ensure the Ollama container is running and healthy. If you are running Ollama on your host machine (not in Docker), you cannot use `localhost` in `config.yml`. Use `host.docker.internal` (Mac/Windows) or the host IP `172.17.0.1` (Linux) and ensure Ollama is listening on `0.0.0.0` (set `OLLAMA_HOST=0.0.0.0` on your host). The provided `docker-compose.ollama.yml` handles this networking automatically by running Ollama in a container.

### API Key Errors
Check that your `.env` file is populated and that the variable names match what is expected in `config.yml` or the default environment variable resolution (e.g., `ANTHROPIC_API_KEY`).

### Permissions
If you encounter permission errors with volumes, ensure the user ID inside the container (default `1000`) has access to the mounted directories.
