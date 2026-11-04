<script setup lang="ts">
import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'

import AppLanguageMenu from '@/components/AppLanguageMenu.vue'
import { useAuthStore } from '@/stores/modules/auth'

import LoginBackground from './components/LoginBackground.vue'
import LoginPanel from './components/LoginPanel.vue'
import LoginThemeToggle from './components/LoginThemeToggle.vue'

const { t } = useI18n({ useScope: 'global' })

type LoginRealm = 'admin' | 'key'

const route = useRoute()
const router = useRouter()
const authStore = useAuthStore()

const realm = shallowRef<LoginRealm>(normalizeRealm(window.history.state?.loginMode))
const username = shallowRef('')
const password = shallowRef('')
const apiKey = shallowRef('')
const loginPending = shallowRef(false)

const canSubmit = computed(() => realm.value === 'key'
  ? Boolean(apiKey.value.trim())
  : Boolean(username.value.trim() && password.value))
const loginLoading = computed(() => authStore.loading || loginPending.value)
const submitDisabled = computed(() => loginLoading.value || !canSubmit.value)

async function handleSubmit(): Promise<void> {
  if (!canSubmit.value || loginPending.value)
    return

  loginPending.value = true
  const selectedRealm = realm.value
  const result = await authStore.login(selectedRealm === 'key'
    ? {
        mode: 'key',
        apiKey: apiKey.value.trim(),
      }
    : {
        mode: 'admin',
        username: username.value.trim(),
        password: password.value,
      })

  if (!result) {
    loginPending.value = false
    return
  }

  try {
    if (result.role === 'admin')
      await router.push(resolveDestination())
    else
      await router.push('/key-usage')
  }
  finally {
    if (router.currentRoute.value.path === '/login')
      loginPending.value = false
  }
}

function normalizeRealm(value: unknown): LoginRealm {
  return value === 'key'
    ? 'key'
    : 'admin'
}

function resolveDestination(): string {
  const requested = route.query.redirect
  if (typeof requested !== 'string' || !requested.startsWith('/') || requested.startsWith('//'))
    return '/'
  return requested
}
</script>

<template>
  <main class="login-page relative isolate min-h-dvh overflow-hidden text-(--z-login-title-color)">
    <LoginBackground />

    <div class="absolute top-5 right-5 z-20 flex items-center gap-2 max-[560px]:top-4 max-[560px]:right-4">
      <AppLanguageMenu size="default" class="[&>button:hover]:bg-z-bg-text-hover" />
      <LoginThemeToggle />
    </div>

    <section
      class="grid min-h-dvh items-center justify-items-center px-5 py-16 min-[980px]:justify-items-end min-[980px]:py-[clamp(24px,5dvh,64px)] min-[980px]:pr-[clamp(48px,17.3vw,332px)] max-[560px]:px-4.5"
      :aria-label="t('auth.pageLabel')"
    >
      <div class="w-[min(440px,100%)] min-[980px]:min-h-120">
        <LoginPanel
          v-model:realm="realm"
          v-model:username="username"
          v-model:password="password"
          v-model:api-key="apiKey"
          :loading="loginLoading"
          :submit-disabled="submitDisabled"
          @submit="handleSubmit"
        />
      </div>
    </section>
  </main>
</template>

<style scoped>
.login-page {
  --z-login-page-bg-start: color-mix(in srgb, var(--z-color-bg-layout) 82%, var(--z-color-white));
  --z-login-page-bg-middle: color-mix(in srgb, var(--z-color-bg-layout) 72%, var(--z-control-item-bg-active));
  --z-login-page-bg-end: color-mix(in srgb, var(--z-color-fill-tertiary) 78%, var(--z-control-item-bg-active));
  --z-login-canvas-bg-start: var(--z-color-bg-container);
  --z-login-canvas-bg-middle: color-mix(in srgb, var(--z-color-bg-layout) 70%, var(--z-color-white));
  --z-login-canvas-bg-end: color-mix(in srgb, var(--z-color-fill-tertiary) 78%, var(--z-control-item-bg-active));
  --z-login-edge-color-middle: color-mix(in srgb, var(--z-color-border) 55%, transparent);
  --z-login-edge-color-end: color-mix(in srgb, var(--z-color-text-secondary) 32%, transparent);
  --z-login-grid-color: color-mix(in srgb, var(--z-color-text-secondary) 5%, transparent);
  --z-login-striation-color: color-mix(in srgb, var(--z-color-text-secondary) 18%, transparent);
  --z-login-grain-color: color-mix(in srgb, var(--z-color-text-secondary) 13%, transparent);
  --z-login-grain-primary-color: color-mix(in srgb, var(--z-color-primary-text) 11%, transparent);
  --z-login-grain-secondary-color: color-mix(in srgb, var(--z-color-text-tertiary) 9%, transparent);
  --z-login-route-bundle-color: color-mix(in srgb, var(--z-color-text-secondary) 20%, transparent);
  --z-login-route-stream-color: color-mix(in srgb, var(--z-color-primary-text) 17%, transparent);
  --z-login-route-audit-color: color-mix(in srgb, var(--z-color-text-tertiary) 18%, transparent);
  --z-login-semantic-color: color-mix(in srgb, var(--z-color-primary-text) 26%, transparent);
  --z-login-particle-color: var(--z-color-primary-text);
  --z-login-particle-glow-color: color-mix(in srgb, var(--z-color-primary-text) 62%, transparent);
  --z-login-watermark-color: color-mix(in srgb, var(--z-color-text) 55%, transparent);
  --z-login-stack-text-color: color-mix(in srgb, var(--z-color-text) 84%, transparent);
  --z-login-stack-bg: color-mix(in srgb, var(--z-color-bg-container) 72%, transparent);
  --z-login-stack-dot-color: color-mix(in srgb, var(--z-color-primary-text) 62%, transparent);
  --z-login-stack-opacity: 0.55;
  --z-login-stack-upstream-opacity: 0.62;
  --z-login-cluster-bg: color-mix(in srgb, var(--z-color-bg-container) 72%, transparent);
  --z-login-cluster-pulse-color: color-mix(in srgb, var(--z-color-primary-text) 66%, transparent);
  --z-login-cluster-text-color: color-mix(in srgb, var(--z-color-text) 78%, transparent);
  --z-login-panel-bg-start: color-mix(in srgb, var(--z-color-bg-container) 95%, transparent);
  --z-login-panel-bg-middle: color-mix(in srgb, var(--z-color-bg-container) 82%, var(--z-control-item-bg-active));
  --z-login-panel-bg-end: color-mix(in srgb, var(--z-color-fill-tertiary) 78%, var(--z-control-item-bg-active));
  --z-login-panel-shadow-color: color-mix(in srgb, var(--z-color-text) 20%, transparent);
  --z-login-panel-line-color: color-mix(in srgb, var(--z-color-primary-border) 54%, transparent);
  --z-login-logo-color: var(--z-color-primary-text);
  --z-login-title-color: var(--z-color-text);
  --z-login-brand-title-color: var(--z-color-text-heading);
  --z-login-brand-caption-color: var(--z-color-text-secondary);
  --z-login-description-color: color-mix(in srgb, var(--z-color-text-secondary) 82%, var(--z-color-text));
  --z-login-label-color: color-mix(in srgb, var(--z-color-text) 84%, var(--z-color-text-secondary));
  --z-login-input-bg: color-mix(in srgb, var(--z-input-bg) 82%, var(--z-color-bg-container));
  --z-login-input-hover-bg: color-mix(in srgb, var(--z-input-hover-bg) 86%, var(--z-color-bg-container));
  --z-login-input-active-bg: var(--z-input-active-bg);
  --z-login-input-icon-color: var(--z-color-primary-text);
  --z-login-placeholder-color: var(--z-color-text-secondary);
  --z-login-error-bg: var(--z-color-error-container);
  --z-login-error-icon-color: var(--z-color-error);
  --z-login-error-text-color: var(--z-color-error-text);
  --z-login-button-shadow-color: color-mix(in srgb, var(--z-color-primary) 30%, transparent);
  --z-login-footer-color: color-mix(in srgb, var(--z-color-text-secondary) 82%, var(--z-color-text-tertiary));
  --z-login-toggle-bg: color-mix(in srgb, var(--z-color-fill-tertiary) 90%, var(--z-color-bg-container));
  --z-login-toggle-bg-hover: var(--z-color-fill-tertiary);
  --z-login-toggle-sun-color: var(--z-login-logo-color);
  --z-login-toggle-moon-color: var(--z-color-text-secondary);
  --z-login-toggle-knob: var(--z-color-bg-container);
  --z-login-toggle-shadow-color: color-mix(in srgb, var(--z-color-primary-text) 20%, transparent);
  --z-login-disabled-bg: var(--z-color-bg-container-disabled);
  --z-login-disabled-text-color: var(--z-color-text-disabled);

  background: linear-gradient(
    118deg,
    var(--z-login-page-bg-start),
    var(--z-login-page-bg-middle) 52%,
    var(--z-login-page-bg-end)
  );
  isolation: isolate;
}

:global(html[data-theme='dark'] .login-page) {
  --z-login-page-bg-start: color-mix(in srgb, var(--z-color-bg-layout) 86%, var(--z-color-bg-spotlight));
  --z-login-page-bg-middle: color-mix(in srgb, var(--z-color-bg-container) 68%, var(--z-color-bg-layout));
  --z-login-page-bg-end: color-mix(in srgb, var(--z-color-bg-spotlight) 76%, var(--z-color-primary-container));
  --z-login-canvas-bg-start: color-mix(in srgb, var(--z-color-bg-container) 82%, var(--z-color-fill-tertiary));
  --z-login-canvas-bg-middle: color-mix(in srgb, var(--z-color-bg-layout) 88%, var(--z-color-bg-container));
  --z-login-canvas-bg-end: color-mix(in srgb, var(--z-color-bg-spotlight) 88%, var(--z-color-bg-layout));
  --z-login-edge-color-middle: color-mix(in srgb, var(--z-color-bg-spotlight) 32%, transparent);
  --z-login-edge-color-end: color-mix(in srgb, var(--z-color-bg-spotlight) 66%, transparent);
  --z-login-grid-color: color-mix(in srgb, var(--z-color-primary-text) 5%, transparent);
  --z-login-striation-color: color-mix(in srgb, var(--z-color-primary-text) 5%, transparent);
  --z-login-grain-color: color-mix(in srgb, var(--z-color-text-secondary) 9%, transparent);
  --z-login-grain-primary-color: color-mix(in srgb, var(--z-color-primary-text) 7%, transparent);
  --z-login-grain-secondary-color: color-mix(in srgb, var(--z-color-text-tertiary) 5%, transparent);
  --z-login-route-bundle-color: color-mix(in srgb, var(--z-color-primary-text) 17%, transparent);
  --z-login-route-stream-color: color-mix(in srgb, var(--z-color-primary-text) 14%, transparent);
  --z-login-route-audit-color: color-mix(in srgb, var(--z-color-text-secondary) 12%, transparent);
  --z-login-semantic-color: color-mix(in srgb, var(--z-color-primary-text) 27%, transparent);
  --z-login-particle-color: color-mix(in srgb, var(--z-color-primary-text) 72%, var(--z-color-primary));
  --z-login-particle-glow-color: color-mix(in srgb, var(--z-color-primary-text) 80%, transparent);
  --z-login-watermark-color: color-mix(in srgb, var(--z-color-primary-text) 40%, transparent);
  --z-login-stack-text-color: color-mix(in srgb, var(--z-color-text) 80%, transparent);
  --z-login-stack-bg: color-mix(in srgb, var(--z-color-bg-container) 54%, transparent);
  --z-login-stack-dot-color: color-mix(in srgb, var(--z-color-primary-text) 72%, transparent);
  --z-login-stack-opacity: 0.76;
  --z-login-stack-upstream-opacity: 0.76;
  --z-login-cluster-bg: color-mix(in srgb, var(--z-color-bg-container) 52%, transparent);
  --z-login-cluster-pulse-color: color-mix(in srgb, var(--z-color-primary-text) 70%, transparent);
  --z-login-cluster-text-color: color-mix(in srgb, var(--z-color-text) 66%, transparent);
  --z-login-panel-bg-start: color-mix(in srgb, var(--z-color-fill-tertiary) 82%, transparent);
  --z-login-panel-bg-middle: color-mix(in srgb, var(--z-color-bg-container) 86%, transparent);
  --z-login-panel-bg-end: color-mix(in srgb, var(--z-color-bg-layout) 90%, transparent);
  --z-login-panel-shadow-color: color-mix(in srgb, var(--z-color-bg-spotlight) 70%, transparent);
  --z-login-panel-line-color: color-mix(in srgb, var(--z-color-primary-border) 66%, transparent);
  --z-login-logo-color: var(--z-color-primary-text);
  --z-login-title-color: var(--z-color-text-heading);
  --z-login-brand-title-color: var(--z-color-white);
  --z-login-brand-caption-color: var(--z-color-text-tertiary);
  --z-login-description-color: color-mix(in srgb, var(--z-color-text-secondary) 82%, var(--z-color-text));
  --z-login-label-color: color-mix(in srgb, var(--z-color-text) 86%, var(--z-color-white));
  --z-login-input-bg: color-mix(in srgb, var(--z-color-bg-spotlight) 58%, transparent);
  --z-login-input-hover-bg: color-mix(in srgb, var(--z-color-fill-tertiary) 62%, transparent);
  --z-login-input-active-bg: color-mix(in srgb, var(--z-color-fill-tertiary) 72%, transparent);
  --z-login-input-icon-color: var(--z-color-primary-text);
  --z-login-placeholder-color: var(--z-color-text-quaternary);
  --z-login-button-shadow-color: color-mix(in srgb, var(--z-color-primary) 30%, transparent);
  --z-login-footer-color: color-mix(in srgb, var(--z-color-text-secondary) 80%, var(--z-color-text-quaternary));
  --z-login-toggle-bg: var(--z-color-fill-tertiary);
  --z-login-toggle-bg-hover: color-mix(in srgb, var(--z-color-fill-tertiary) 94%, var(--z-color-white));
  --z-login-toggle-sun-color: var(--z-color-text-tertiary);
  --z-login-toggle-moon-color: var(--z-color-white);
  --z-login-toggle-knob: var(--z-color-primary-hover);
  --z-login-toggle-shadow-color: color-mix(in srgb, var(--z-color-primary-text) 30%, transparent);
}
</style>
