<script setup lang="ts">
import { storeToRefs } from 'pinia'
import { computed, shallowRef } from 'vue'
import { useRouter } from 'vue-router'

import { useAuthStore } from '@/stores/modules/auth'
import { useThemeStore } from '@/stores/modules/theme'

import LoginBackground from './components/LoginBackground.vue'
import LoginPanel from './components/LoginPanel.vue'

const router = useRouter()
const authStore = useAuthStore()
const themeStore = useThemeStore()
const { effectiveTheme } = storeToRefs(themeStore)
const { toggleTheme } = themeStore

const username = shallowRef('')
const password = shallowRef('')
const loginPending = shallowRef(false)
const canSubmit = computed<boolean>(() => !!username.value.trim() && !!password.value.trim())
const loginLoading = computed<boolean>(() => authStore.loading || loginPending.value)
const submitDisabled = computed<boolean>(() => loginLoading.value || !canSubmit.value)

async function handleSubmit(): Promise<void> {
  if (!canSubmit.value || loginPending.value) {
    return
  }

  loginPending.value = true
  const success = await authStore.login({
    username: username.value.trim(),
    password: password.value,
  })

  if (!success) {
    loginPending.value = false
    return
  }

  try {
    await router.push('/')
  }
  finally {
    // 成功时登录页会卸载；导航失败并停留当前页时才恢复按钮。
    if (router.currentRoute.value.path === '/login') {
      loginPending.value = false
    }
  }
}
</script>

<template>
  <main class="login-page relative isolate min-h-dvh overflow-hidden text-(--cp-login-title-color)">
    <LoginBackground />

    <section
      class="grid min-h-dvh items-center justify-items-center px-5 py-[clamp(24px,5dvh,64px)] min-[980px]:justify-items-end min-[980px]:pr-[clamp(48px,17.3vw,332px)] max-[560px]:p-4.5"
      aria-label="Codex Proxy RS 登录"
    >
      <LoginPanel
        v-model:username="username"
        v-model:password="password"
        :error="authStore.error"
        :loading="loginLoading"
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
  --cp-login-page-bg-start: color-mix(in srgb, var(--cp-color-bg-layout) 82%, var(--cp-color-white));
  --cp-login-page-bg-middle: color-mix(in srgb, var(--cp-color-bg-layout) 72%, var(--cp-control-item-bg-active));
  --cp-login-page-bg-end: color-mix(in srgb, var(--cp-color-fill-tertiary) 78%, var(--cp-control-item-bg-active));
  --cp-login-canvas-bg-start: var(--cp-color-bg-container);
  --cp-login-canvas-bg-middle: color-mix(in srgb, var(--cp-color-bg-layout) 70%, var(--cp-color-white));
  --cp-login-canvas-bg-end: color-mix(in srgb, var(--cp-color-fill-tertiary) 78%, var(--cp-control-item-bg-active));
  --cp-login-edge-color-middle: color-mix(in srgb, var(--cp-color-border) 55%, transparent);
  --cp-login-edge-color-end: color-mix(in srgb, var(--cp-color-text-secondary) 32%, transparent);
  --cp-login-grid-color: color-mix(in srgb, var(--cp-color-text-secondary) 5%, transparent);
  --cp-login-striation-color: color-mix(in srgb, var(--cp-color-text-secondary) 18%, transparent);
  --cp-login-grain-color: color-mix(in srgb, var(--cp-color-text-secondary) 13%, transparent);
  --cp-login-grain-primary-color: color-mix(in srgb, var(--cp-color-primary-text) 11%, transparent);
  --cp-login-grain-secondary-color: color-mix(in srgb, var(--cp-color-text-tertiary) 9%, transparent);
  --cp-login-route-bundle-color: color-mix(in srgb, var(--cp-color-text-secondary) 20%, transparent);
  --cp-login-route-stream-color: color-mix(in srgb, var(--cp-color-primary-text) 17%, transparent);
  --cp-login-route-audit-color: color-mix(in srgb, var(--cp-color-text-tertiary) 18%, transparent);
  --cp-login-semantic-color: color-mix(in srgb, var(--cp-color-primary-text) 26%, transparent);
  --cp-login-particle-color: var(--cp-color-primary-text);
  --cp-login-particle-glow-color: color-mix(in srgb, var(--cp-color-primary-text) 62%, transparent);
  --cp-login-watermark-color: color-mix(in srgb, var(--cp-color-text) 55%, transparent);
  --cp-login-stack-text-color: color-mix(in srgb, var(--cp-color-text) 84%, transparent);
  --cp-login-stack-bg: color-mix(in srgb, var(--cp-color-bg-container) 72%, transparent);
  --cp-login-stack-dot-color: color-mix(in srgb, var(--cp-color-primary-text) 62%, transparent);
  --cp-login-stack-opacity: 0.55;
  --cp-login-stack-upstream-opacity: 0.62;
  --cp-login-cluster-bg: color-mix(in srgb, var(--cp-color-bg-container) 72%, transparent);
  --cp-login-cluster-pulse-color: color-mix(in srgb, var(--cp-color-primary-text) 66%, transparent);
  --cp-login-cluster-text-color: color-mix(in srgb, var(--cp-color-text) 78%, transparent);
  --cp-login-panel-bg-start: color-mix(in srgb, var(--cp-color-bg-container) 95%, transparent);
  --cp-login-panel-bg-middle: color-mix(in srgb, var(--cp-color-bg-container) 82%, var(--cp-control-item-bg-active));
  --cp-login-panel-bg-end: color-mix(in srgb, var(--cp-color-fill-tertiary) 78%, var(--cp-control-item-bg-active));
  --cp-login-panel-shadow-color: color-mix(in srgb, var(--cp-color-text) 20%, transparent);
  --cp-login-panel-line-color: color-mix(in srgb, var(--cp-color-primary-border) 54%, transparent);
  --cp-login-logo-color: var(--cp-color-primary-text);
  --cp-login-title-color: var(--cp-color-text);
  --cp-login-brand-title-color: var(--cp-color-text-heading);
  --cp-login-brand-caption-color: var(--cp-color-text-secondary);
  --cp-login-description-color: color-mix(in srgb, var(--cp-color-text-secondary) 82%, var(--cp-color-text));
  --cp-login-label-color: color-mix(in srgb, var(--cp-color-text) 84%, var(--cp-color-text-secondary));
  --cp-login-input-bg: color-mix(in srgb, var(--cp-input-bg) 82%, var(--cp-color-bg-container));
  --cp-login-input-hover-bg: color-mix(in srgb, var(--cp-input-hover-bg) 86%, var(--cp-color-bg-container));
  --cp-login-input-active-bg: var(--cp-input-active-bg);
  --cp-login-input-icon-color: var(--cp-color-primary-text);
  --cp-login-placeholder-color: var(--cp-color-text-secondary);
  --cp-login-error-bg: var(--cp-color-error-bg);
  --cp-login-error-icon-color: var(--cp-color-error);
  --cp-login-error-text-color: var(--cp-color-error-text);
  --cp-login-button-shadow-color: color-mix(in srgb, var(--cp-color-primary) 30%, transparent);
  --cp-login-footer-color: color-mix(in srgb, var(--cp-color-text-secondary) 82%, var(--cp-color-text-tertiary));
  --cp-login-toggle-bg: color-mix(in srgb, var(--cp-color-fill-tertiary) 90%, var(--cp-color-bg-container));
  --cp-login-toggle-bg-hover: var(--cp-color-fill-tertiary);
  --cp-login-toggle-sun-color: var(--cp-login-logo-color);
  --cp-login-toggle-moon-color: var(--cp-color-text-secondary);
  --cp-login-toggle-knob: var(--cp-color-bg-container);
  --cp-login-toggle-shadow-color: color-mix(in srgb, var(--cp-color-primary-text) 20%, transparent);
  --cp-login-disabled-bg: var(--cp-color-bg-container-disabled);
  --cp-login-disabled-text-color: var(--cp-color-text-disabled);

  background: linear-gradient(
    118deg,
    var(--cp-login-page-bg-start),
    var(--cp-login-page-bg-middle) 52%,
    var(--cp-login-page-bg-end)
  );
  isolation: isolate;
}

:global(html[data-theme='dark'] .login-page) {
  --cp-login-page-bg-start: color-mix(in srgb, var(--cp-color-bg-layout) 86%, var(--cp-color-bg-spotlight));
  --cp-login-page-bg-middle: color-mix(in srgb, var(--cp-color-bg-container) 68%, var(--cp-color-bg-layout));
  --cp-login-page-bg-end: color-mix(in srgb, var(--cp-color-bg-spotlight) 76%, var(--cp-color-primary-bg));
  --cp-login-canvas-bg-start: color-mix(in srgb, var(--cp-color-bg-container) 82%, var(--cp-color-fill-tertiary));
  --cp-login-canvas-bg-middle: color-mix(in srgb, var(--cp-color-bg-layout) 88%, var(--cp-color-bg-container));
  --cp-login-canvas-bg-end: color-mix(in srgb, var(--cp-color-bg-spotlight) 88%, var(--cp-color-bg-layout));
  --cp-login-edge-color-middle: color-mix(in srgb, var(--cp-color-bg-spotlight) 32%, transparent);
  --cp-login-edge-color-end: color-mix(in srgb, var(--cp-color-bg-spotlight) 66%, transparent);
  --cp-login-grid-color: color-mix(in srgb, var(--cp-color-primary-text) 5%, transparent);
  --cp-login-striation-color: color-mix(in srgb, var(--cp-color-primary-text) 5%, transparent);
  --cp-login-grain-color: color-mix(in srgb, var(--cp-color-text-secondary) 9%, transparent);
  --cp-login-grain-primary-color: color-mix(in srgb, var(--cp-color-primary-text) 7%, transparent);
  --cp-login-grain-secondary-color: color-mix(in srgb, var(--cp-color-text-tertiary) 5%, transparent);
  --cp-login-route-bundle-color: color-mix(in srgb, var(--cp-color-primary-text) 17%, transparent);
  --cp-login-route-stream-color: color-mix(in srgb, var(--cp-color-primary-text) 14%, transparent);
  --cp-login-route-audit-color: color-mix(in srgb, var(--cp-color-text-secondary) 12%, transparent);
  --cp-login-semantic-color: color-mix(in srgb, var(--cp-color-primary-text) 27%, transparent);
  --cp-login-particle-color: color-mix(in srgb, var(--cp-color-primary-text) 72%, var(--cp-color-primary));
  --cp-login-particle-glow-color: color-mix(in srgb, var(--cp-color-primary-text) 80%, transparent);
  --cp-login-watermark-color: color-mix(in srgb, var(--cp-color-primary-text) 40%, transparent);
  --cp-login-stack-text-color: color-mix(in srgb, var(--cp-color-text) 80%, transparent);
  --cp-login-stack-bg: color-mix(in srgb, var(--cp-color-bg-container) 54%, transparent);
  --cp-login-stack-dot-color: color-mix(in srgb, var(--cp-color-primary-text) 72%, transparent);
  --cp-login-stack-opacity: 0.76;
  --cp-login-stack-upstream-opacity: 0.76;
  --cp-login-cluster-bg: color-mix(in srgb, var(--cp-color-bg-container) 52%, transparent);
  --cp-login-cluster-pulse-color: color-mix(in srgb, var(--cp-color-primary-text) 70%, transparent);
  --cp-login-cluster-text-color: color-mix(in srgb, var(--cp-color-text) 66%, transparent);
  --cp-login-panel-bg-start: color-mix(in srgb, var(--cp-color-fill-tertiary) 82%, transparent);
  --cp-login-panel-bg-middle: color-mix(in srgb, var(--cp-color-bg-container) 86%, transparent);
  --cp-login-panel-bg-end: color-mix(in srgb, var(--cp-color-bg-layout) 90%, transparent);
  --cp-login-panel-shadow-color: color-mix(in srgb, var(--cp-color-bg-spotlight) 70%, transparent);
  --cp-login-panel-line-color: color-mix(in srgb, var(--cp-color-primary-border) 66%, transparent);
  --cp-login-logo-color: var(--cp-color-primary-text);
  --cp-login-title-color: var(--cp-color-text-heading);
  --cp-login-brand-title-color: var(--cp-color-white);
  --cp-login-brand-caption-color: var(--cp-color-text-tertiary);
  --cp-login-description-color: color-mix(in srgb, var(--cp-color-text-secondary) 82%, var(--cp-color-text));
  --cp-login-label-color: color-mix(in srgb, var(--cp-color-text) 86%, var(--cp-color-white));
  --cp-login-input-bg: color-mix(in srgb, var(--cp-color-bg-spotlight) 58%, transparent);
  --cp-login-input-hover-bg: color-mix(in srgb, var(--cp-color-fill-tertiary) 62%, transparent);
  --cp-login-input-active-bg: color-mix(in srgb, var(--cp-color-fill-tertiary) 72%, transparent);
  --cp-login-input-icon-color: var(--cp-color-primary-text);
  --cp-login-placeholder-color: var(--cp-color-text-quaternary);
  --cp-login-button-shadow-color: color-mix(in srgb, var(--cp-color-primary) 30%, transparent);
  --cp-login-footer-color: color-mix(in srgb, var(--cp-color-text-secondary) 80%, var(--cp-color-text-quaternary));
  --cp-login-toggle-bg: var(--cp-color-fill-tertiary);
  --cp-login-toggle-bg-hover: color-mix(in srgb, var(--cp-color-fill-tertiary) 94%, var(--cp-color-white));
  --cp-login-toggle-sun-color: var(--cp-color-text-tertiary);
  --cp-login-toggle-moon-color: var(--cp-color-white);
  --cp-login-toggle-knob: var(--cp-color-primary-hover);
  --cp-login-toggle-shadow-color: color-mix(in srgb, var(--cp-color-primary-text) 30%, transparent);
}
</style>
