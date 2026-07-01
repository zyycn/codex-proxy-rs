<script setup lang="ts">
import { storeToRefs } from 'pinia'
import { computed, shallowRef } from 'vue'
import { useRouter } from 'vue-router'

import { useAuthStore } from '@/stores/modules/auth'
import { useUiStore } from '@/stores/modules/ui'

import LoginBackground from './components/LoginBackground.vue'
import LoginPanel from './components/LoginPanel.vue'

const router = useRouter()
const authStore = useAuthStore()
const uiStore = useUiStore()
const { effectiveTheme } = storeToRefs(uiStore)
const { toggleTheme } = uiStore

const username = shallowRef('')
const password = shallowRef('')
const canSubmit = computed<boolean>(() => !!username.value.trim() && !!password.value.trim())
const submitDisabled = computed<boolean>(() => authStore.loading || !canSubmit.value)

async function handleSubmit(): Promise<void> {
  if (!canSubmit.value) {
    return
  }

  const success = await authStore.login({
    username: username.value.trim(),
    password: password.value,
  })

  if (success) {
    router.push('/')
  }
}
</script>

<template>
  <main class="login-page relative isolate min-h-dvh overflow-hidden text-(--login-title)">
    <LoginBackground />

    <section
      class="grid min-h-dvh items-center justify-items-center px-5 py-[clamp(24px,5vh,64px)] min-[980px]:justify-items-end min-[980px]:pr-[clamp(48px,17.3vw,332px)] max-[560px]:p-4.5"
      aria-label="Codex Proxy RS 登录"
    >
      <LoginPanel
        v-model:username="username"
        v-model:password="password"
        :error="authStore.error"
        :loading="authStore.loading"
        :submit-disabled="submitDisabled"
        :effective-theme="effectiveTheme"
        @submit="handleSubmit"
        @toggle-theme="toggleTheme"
      />
    </section>
  </main>
</template>

<style scoped>
.login-page {
  --login-page-a: color-mix(in srgb, var(--cp-bg-page) 82%, var(--cp-white));
  --login-page-b: color-mix(in srgb, var(--cp-bg-page) 72%, var(--cp-info-bg));
  --login-page-c: color-mix(in srgb, var(--cp-bg-muted) 78%, var(--cp-info-bg));
  --login-base-a: var(--cp-bg-surface);
  --login-base-b: color-mix(in srgb, var(--cp-bg-page) 70%, var(--cp-white));
  --login-base-c: color-mix(in srgb, var(--cp-bg-muted) 78%, var(--cp-info-bg));
  --login-edge-mid: color-mix(in srgb, var(--cp-border-primary) 55%, transparent);
  --login-edge-end: color-mix(in srgb, var(--cp-text-secondary) 32%, transparent);
  --login-grid: color-mix(in srgb, var(--cp-text-secondary) 5%, transparent);
  --login-striation: color-mix(in srgb, var(--cp-text-secondary) 18%, transparent);
  --login-grain: color-mix(in srgb, var(--cp-text-secondary) 13%, transparent);
  --login-grain-alt-a: color-mix(in srgb, var(--cp-info) 11%, transparent);
  --login-grain-alt-b: color-mix(in srgb, var(--cp-normal) 9%, transparent);
  --login-route-bundle: color-mix(in srgb, var(--cp-text-secondary) 20%, transparent);
  --login-route-stream: color-mix(in srgb, var(--cp-info) 17%, transparent);
  --login-route-audit: color-mix(in srgb, var(--cp-text-tertiary) 18%, transparent);
  --login-semantic: color-mix(in srgb, var(--cp-info) 26%, transparent);
  --login-particle: color-mix(in srgb, var(--cp-info) 76%, var(--cp-normal));
  --login-particle-glow: color-mix(in srgb, var(--cp-info) 62%, transparent);
  --login-watermark: color-mix(in srgb, var(--cp-text-primary) 55%, transparent);
  --login-stack-text: color-mix(in srgb, var(--cp-text-primary) 84%, transparent);
  --login-stack-bg: color-mix(in srgb, var(--cp-bg-surface) 72%, transparent);
  --login-stack-dot: color-mix(in srgb, var(--cp-info) 62%, transparent);
  --login-stack-opacity: 0.55;
  --login-stack-upstream-opacity: 0.62;
  --login-cluster-bg: color-mix(in srgb, var(--cp-bg-surface) 72%, transparent);
  --login-cluster-pulse: color-mix(in srgb, var(--cp-info) 66%, transparent);
  --login-cluster-text: color-mix(in srgb, var(--cp-text-primary) 78%, transparent);
  --login-form-bg-a: color-mix(in srgb, var(--cp-bg-surface) 95%, transparent);
  --login-form-bg-b: color-mix(in srgb, var(--cp-bg-surface) 82%, var(--cp-info-bg));
  --login-form-bg-c: color-mix(in srgb, var(--cp-bg-muted) 78%, var(--cp-info-bg));
  --login-form-shadow: color-mix(in srgb, var(--cp-text-primary) 20%, transparent);
  --login-form-line: color-mix(in srgb, var(--cp-info-border) 54%, transparent);
  --login-logo-bg: color-mix(in srgb, var(--cp-info-bg) 74%, var(--cp-bg-surface));
  --login-logo-text: color-mix(in srgb, var(--cp-info) 72%, var(--cp-normal));
  --login-title: var(--cp-text-primary);
  --login-brand-title: var(--cp-text-strong);
  --login-brand-caption: var(--cp-text-secondary);
  --login-description: color-mix(in srgb, var(--cp-text-secondary) 82%, var(--cp-text-primary));
  --login-label: color-mix(in srgb, var(--cp-text-primary) 84%, var(--cp-text-secondary));
  --login-input-bg: color-mix(in srgb, var(--cp-input-soft-bg) 82%, var(--cp-bg-surface));
  --login-input-bg-hover: color-mix(
    in srgb,
    var(--cp-input-soft-bg-hover) 86%,
    var(--cp-bg-surface)
  );
  --login-input-focus: var(--cp-input-soft-bg-focus);
  --login-input-icon: var(--cp-info);
  --login-placeholder: var(--cp-text-secondary);
  --login-error-bg: var(--cp-danger-bg);
  --login-error-icon: var(--cp-danger);
  --login-error-text: var(--cp-danger-text);
  --login-button: color-mix(in srgb, var(--cp-info) 74%, var(--cp-normal));
  --login-button-hover: color-mix(in srgb, var(--cp-info-hover) 74%, var(--cp-normal-hover));
  --login-button-active: color-mix(in srgb, var(--cp-info-pressed) 74%, var(--cp-normal-pressed));
  --login-button-shadow: color-mix(in srgb, var(--cp-info) 30%, transparent);
  --login-footer: color-mix(in srgb, var(--cp-text-secondary) 82%, var(--cp-text-tertiary));
  --login-toggle-bg: color-mix(in srgb, var(--cp-bg-muted) 74%, transparent);
  --login-toggle-sun: var(--login-logo-text);
  --login-toggle-moon: var(--cp-text-secondary);
  --login-toggle-knob: var(--cp-bg-surface);
  --login-toggle-shadow: color-mix(in srgb, var(--cp-info) 20%, transparent);
  --login-disabled-bg: var(--cp-disabled-bg);
  --login-disabled-text: var(--cp-disabled-text);

  background: linear-gradient(
    118deg,
    var(--login-page-a),
    var(--login-page-b) 52%,
    var(--login-page-c)
  );
  isolation: isolate;
}

:global(html[data-theme='dark'] .login-page) {
  --login-page-a: color-mix(in srgb, var(--cp-bg-page) 86%, var(--cp-bg-dark));
  --login-page-b: color-mix(in srgb, var(--cp-bg-surface) 68%, var(--cp-bg-page));
  --login-page-c: color-mix(in srgb, var(--cp-bg-dark) 76%, var(--cp-normal-bg));
  --login-base-a: color-mix(in srgb, var(--cp-bg-surface) 82%, var(--cp-bg-muted));
  --login-base-b: color-mix(in srgb, var(--cp-bg-page) 88%, var(--cp-bg-surface));
  --login-base-c: color-mix(in srgb, var(--cp-bg-dark) 88%, var(--cp-bg-page));
  --login-edge-mid: color-mix(in srgb, var(--cp-bg-dark) 32%, transparent);
  --login-edge-end: color-mix(in srgb, var(--cp-bg-dark) 66%, transparent);
  --login-grid: color-mix(in srgb, var(--cp-info-text) 5%, transparent);
  --login-striation: color-mix(in srgb, var(--cp-info-text) 5%, transparent);
  --login-grain: color-mix(in srgb, var(--cp-text-secondary) 9%, transparent);
  --login-grain-alt-a: color-mix(in srgb, var(--cp-info-text) 7%, transparent);
  --login-grain-alt-b: color-mix(in srgb, var(--cp-normal-text) 5%, transparent);
  --login-route-bundle: color-mix(in srgb, var(--cp-info-text) 17%, transparent);
  --login-route-stream: color-mix(in srgb, var(--cp-info-text) 14%, transparent);
  --login-route-audit: color-mix(in srgb, var(--cp-text-secondary) 12%, transparent);
  --login-semantic: color-mix(in srgb, var(--cp-info-text) 27%, transparent);
  --login-particle: color-mix(in srgb, var(--cp-info-text) 72%, var(--cp-normal));
  --login-particle-glow: color-mix(in srgb, var(--cp-info-text) 80%, transparent);
  --login-watermark: color-mix(in srgb, var(--cp-info-text) 40%, transparent);
  --login-stack-text: color-mix(in srgb, var(--cp-text-primary) 80%, transparent);
  --login-stack-bg: color-mix(in srgb, var(--cp-bg-surface) 54%, transparent);
  --login-stack-dot: color-mix(in srgb, var(--cp-info-text) 72%, transparent);
  --login-stack-opacity: 0.76;
  --login-stack-upstream-opacity: 0.76;
  --login-cluster-bg: color-mix(in srgb, var(--cp-bg-surface) 52%, transparent);
  --login-cluster-pulse: color-mix(in srgb, var(--cp-info-text) 70%, transparent);
  --login-cluster-text: color-mix(in srgb, var(--cp-text-primary) 66%, transparent);
  --login-form-bg-a: color-mix(in srgb, var(--cp-bg-muted) 82%, transparent);
  --login-form-bg-b: color-mix(in srgb, var(--cp-bg-surface) 86%, transparent);
  --login-form-bg-c: color-mix(in srgb, var(--cp-bg-page) 90%, transparent);
  --login-form-shadow: color-mix(in srgb, var(--cp-bg-dark) 70%, transparent);
  --login-form-line: color-mix(in srgb, var(--cp-info-text) 66%, transparent);
  --login-logo-bg: color-mix(in srgb, var(--cp-bg-surface) 84%, transparent);
  --login-logo-text: var(--cp-info-text);
  --login-title: var(--cp-text-strong);
  --login-brand-title: var(--cp-white);
  --login-brand-caption: var(--cp-text-tertiary);
  --login-description: color-mix(in srgb, var(--cp-text-secondary) 82%, var(--cp-text-primary));
  --login-label: color-mix(in srgb, var(--cp-text-primary) 86%, var(--cp-white));
  --login-input-bg: color-mix(in srgb, var(--cp-bg-dark) 58%, transparent);
  --login-input-bg-hover: color-mix(in srgb, var(--cp-bg-muted) 62%, transparent);
  --login-input-focus: color-mix(in srgb, var(--cp-bg-muted) 72%, transparent);
  --login-input-icon: var(--cp-info-text);
  --login-placeholder: var(--cp-text-muted);
  --login-button: color-mix(in srgb, var(--cp-info) 76%, var(--cp-normal));
  --login-button-hover: color-mix(in srgb, var(--cp-info-hover) 78%, var(--cp-normal-hover));
  --login-button-active: color-mix(in srgb, var(--cp-info-pressed) 78%, var(--cp-normal-pressed));
  --login-button-shadow: color-mix(in srgb, var(--cp-info) 30%, transparent);
  --login-footer: color-mix(in srgb, var(--cp-text-secondary) 80%, var(--cp-text-muted));
  --login-toggle-bg: color-mix(in srgb, var(--cp-bg-surface) 84%, transparent);
  --login-toggle-sun: var(--cp-text-tertiary);
  --login-toggle-moon: var(--cp-white);
  --login-toggle-knob: var(--login-button-hover);
  --login-toggle-shadow: color-mix(in srgb, var(--cp-info) 30%, transparent);
}
</style>
