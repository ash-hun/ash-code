"""Provider discovery and active-selection.

Discovery order (lowest priority first — later wins):
  1. Built-in modules under ``ashpy.providers.*_p``.
  2. ``providers/*.toml`` files on the mounted volume, keyed by file stem.
     These override the built-in default config.

Selection order for the *active* provider:
  1. Explicit call to ``ProviderRegistry.switch(name, model)``.
  2. ``ASH_LLM_PROVIDER`` environment variable (+ optional
     ``ASH_LLM_MODEL`` override).
  3. The registry's hard-coded fallback: ``anthropic``.

Missing credentials never prevent a provider from being registered; the
provider loads, reports ``HealthState.UNCONFIGURED`` from ``health()``,
and produces a friendly error delta if invoked.
"""

from __future__ import annotations

import importlib
import logging
import os
import pathlib
import threading
from dataclasses import replace
from typing import Optional

try:  # py >= 3.11
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib  # type: ignore

from .base import LlmProvider, ProviderCaps, ProviderConfig

_LOG = logging.getLogger(__name__)

DEFAULT_PROVIDER = "anthropic"

_BUILTIN_SPECS: dict[str, tuple[str, str]] = {
    "anthropic": ("ashpy.providers.anthropic_p", "AnthropicProvider"),
    "openai": ("ashpy.providers.openai_p", "OpenAIProvider"),
    "vllm": ("ashpy.providers.vllm_p", "VllmProvider"),
    "ollama": ("ashpy.providers.ollama_p", "OllamaProvider"),
}


class ProviderRegistry:
    def __init__(self, providers_dir: Optional[pathlib.Path] = None) -> None:
        self._providers_dir = providers_dir or self._default_dir()
        self._lock = threading.RLock()
        self._configs: dict[str, ProviderConfig] = {}
        self._instances: dict[str, LlmProvider] = {}
        self._active: str = DEFAULT_PROVIDER
        self.reload()

    # --- discovery ---------------------------------------------------------

    @staticmethod
    def _default_dir() -> pathlib.Path:
        env = os.environ.get("ASH_PROVIDERS_DIR")
        if env:
            return pathlib.Path(env)
        return pathlib.Path("/root/.ash/providers")

    def reload(self) -> None:
        """Re-scan built-ins and volume-mounted TOML configs."""
        with self._lock:
            self._configs = {}
            self._instances = {}

            # 1) built-ins
            for name, (module, class_name) in _BUILTIN_SPECS.items():
                self._configs[name] = ProviderConfig(
                    name=name,
                    module=module,
                    class_name=class_name,
                    defaults=_builtin_defaults(name),
                    auth=_builtin_auth(name),
                    source="builtin",
                )

            # 2) volume overrides
            if self._providers_dir.is_dir():
                for toml_file in sorted(self._providers_dir.glob("*.toml")):
                    try:
                        data = tomllib.loads(toml_file.read_text(encoding="utf-8"))
                    except Exception as exc:
                        _LOG.warning("failed to parse %s: %s", toml_file, exc)
                        continue
                    cfg = _config_from_toml(data, default_name=toml_file.stem, source=str(toml_file))
                    self._configs[cfg.name] = cfg

            # 3) resolve active provider from env (if set and known)
            env_provider = os.environ.get("ASH_LLM_PROVIDER", "").strip()
            if env_provider and env_provider in self._configs:
                self._active = env_provider
            elif self._active not in self._configs:
                self._active = next(iter(self._configs)) if self._configs else DEFAULT_PROVIDER

            # 4) ASH_LLM_MODEL overrides whatever the active provider's config says
            env_model = os.environ.get("ASH_LLM_MODEL", "").strip()
            if env_model and self._active in self._configs:
                cfg = self._configs[self._active]
                new_defaults = dict(cfg.defaults)
                new_defaults["model"] = env_model
                self._configs[self._active] = replace(cfg, defaults=new_defaults)

    # --- accessors ---------------------------------------------------------

    def list_names(self) -> list[str]:
        with self._lock:
            return sorted(self._configs.keys())

    def configs(self) -> dict[str, ProviderConfig]:
        with self._lock:
            return dict(self._configs)

    def active_name(self) -> str:
        with self._lock:
            return self._active

    def get(self, name: str) -> LlmProvider:
        with self._lock:
            if name in self._instances:
                return self._instances[name]
            cfg = self._configs.get(name)
            if cfg is None:
                raise KeyError(f"unknown provider: {name}")
            instance = _instantiate(cfg)
            self._instances[name] = instance
            return instance

    def current(self) -> LlmProvider:
        return self.get(self.active_name())

    def switch(self, name: str, model: str = "") -> None:
        with self._lock:
            if name not in self._configs:
                raise KeyError(f"unknown provider: {name}")
            self._active = name
            if model:
                cfg = self._configs[name]
                new_defaults = dict(cfg.defaults)
                new_defaults["model"] = model
                self._configs[name] = replace(cfg, defaults=new_defaults)
                # Force re-instantiation so caps() reflects the new model.
                self._instances.pop(name, None)

    def capabilities(self, name: str) -> ProviderCaps:
        return self.get(name).capabilities()


# --- helpers ----------------------------------------------------------------


def _instantiate(cfg: ProviderConfig) -> LlmProvider:
    module = importlib.import_module(cfg.module)
    cls = getattr(module, cfg.class_name)
    instance = cls(cfg)
    if not isinstance(instance, LlmProvider):
        raise TypeError(f"{cfg.module}.{cfg.class_name} does not implement LlmProvider")
    return instance


def _config_from_toml(
    data: dict, default_name: str, source: str
) -> ProviderConfig:
    provider_block = data.get("provider", {}) or {}
    defaults_block = data.get("defaults", {}) or {}
    auth_block = data.get("auth", {}) or {}

    name = provider_block.get("name", default_name)
    if name in _BUILTIN_SPECS and "module" not in provider_block:
        module, class_name = _BUILTIN_SPECS[name]
    else:
        module = provider_block.get("module", "")
        class_name = provider_block.get("class", "")

    merged_defaults = dict(_builtin_defaults(name))
    merged_defaults.update(defaults_block)
    merged_auth = dict(_builtin_auth(name))
    merged_auth.update(auth_block)

    return ProviderConfig(
        name=name,
        module=module,
        class_name=class_name,
        defaults=merged_defaults,
        auth=merged_auth,
        source=f"plugin:{source}",
    )


def _builtin_defaults(name: str) -> dict:
    if name == "anthropic":
        return {"model": "claude-opus-4-6", "temperature": 0.2, "max_tokens": 4096}
    if name == "openai":
        return {"model": "gpt-4.1-mini", "temperature": 0.2, "max_tokens": 4096}
    if name == "vllm":
        return {"model": "", "temperature": 0.2, "max_tokens": 4096}
    if name == "ollama":
        return {"model": "llama3.1", "temperature": 0.2}
    return {}


def _builtin_auth(name: str) -> dict:
    if name == "anthropic":
        return {"api_key_env": "ANTHROPIC_API_KEY", "base_url_env": "ANTHROPIC_BASE_URL"}
    if name == "openai":
        return {"api_key_env": "OPENAI_API_KEY", "base_url_env": "OPENAI_BASE_URL"}
    if name == "vllm":
        return {"api_key_env": "VLLM_API_KEY", "base_url_env": "VLLM_BASE_URL"}
    if name == "ollama":
        return {"base_url_env": "OLLAMA_BASE_URL"}
    return {}


# --- module-level singleton -------------------------------------------------


_REGISTRY: Optional[ProviderRegistry] = None
_REGISTRY_LOCK = threading.Lock()


def get_registry() -> ProviderRegistry:
    global _REGISTRY
    with _REGISTRY_LOCK:
        if _REGISTRY is None:
            _REGISTRY = ProviderRegistry()
        return _REGISTRY


def reset_registry_for_tests(providers_dir: Optional[pathlib.Path] = None) -> ProviderRegistry:
    """Test-only entry point."""
    global _REGISTRY
    with _REGISTRY_LOCK:
        _REGISTRY = ProviderRegistry(providers_dir=providers_dir)
        return _REGISTRY
