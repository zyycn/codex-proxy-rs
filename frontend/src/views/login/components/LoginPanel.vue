<script setup lang="ts">
import { ZButton, ZCard, ZInput, ZMotionIcon, ZSegmented } from '@codex-proxy/ui'

import { KeyRound, Mail, ShieldCheck } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import AppBrandMark from '@/components/AppBrandMark.vue'

const props = defineProps<{
  loading: boolean
  submitDisabled: boolean
}>()

const emit = defineEmits<{
  submit: []
}>()

const { t } = useI18n({ useScope: 'global' })

type LoginRealm = 'admin' | 'key'

const realm = defineModel<LoginRealm>('realm', { required: true })
const username = defineModel<string>('username', { required: true })
const password = defineModel<string>('password', { required: true })
const apiKey = defineModel<string>('apiKey', { required: true })

const realmOptions = computed(() => ([
  {
    label: t('auth.adminSignIn'),
    value: 'admin',
    icon: ShieldCheck,
  },
  {
    label: t('auth.apiKeySignIn'),
    value: 'key',
    icon: KeyRound,
  },
]))

const isKeyRealm = computed(() => realm.value === 'key')
const realmCaption = computed(() => isKeyRealm.value
  ? t('auth.keyRealm')
  : t('auth.adminRealm'))
const submitLabel = computed(() => {
  if (props.loading)
    return t('auth.signingIn')
  return t('auth.signIn')
})
</script>

<template>
  <ZCard
    as="form"
    padding="none"
    class="login-form relative grid w-full content-start gap-8 rounded-lg px-7.5 pt-10 pb-6 max-[560px]:gap-6 max-[560px]:p-5.5"
    @submit.prevent="emit('submit')"
  >
    <div class="login-form-line" />

    <header class="flex min-w-0 items-center justify-between gap-4.5 max-[560px]:gap-3.5">
      <div class="flex min-w-0 items-center gap-2">
        <ZMotionIcon variant="brand" class="login-logo">
          <AppBrandMark class="block size-10.5 select-none" />
        </ZMotionIcon>
        <span class="grid min-w-0 gap-1">
          <strong
            class="text-[17px] leading-[1.3] font-semibold text-(--cp-login-brand-title-color) max-[560px]:text-cp-xl"
          >
            Codex Proxy RS
          </strong>
          <span class="font-mono text-[10px] leading-[1.2] font-normal text-(--cp-login-brand-caption-color) ml-0.5">
            {{ realmCaption }}
          </span>
        </span>
      </div>

      <ZSegmented
        v-model="realm"
        class="w-18 shrink-0 [--cp-color-bg-container:var(--cp-color-bg-elevated)] [--cp-color-fill-tertiary:var(--cp-login-input-bg)] [--cp-control-height-sm:34px] [&_svg]:size-4"
        :aria-label="t('auth.method')"
        :options="realmOptions"
        display="icon"
        size="small"
        :disabled="loading"
      />
    </header>

    <section class="grid min-h-18 min-w-0 content-start gap-4" aria-labelledby="login-title">
      <h1
        id="login-title"
        class="m-0 text-[34px] leading-[1.02] font-semibold text-(--cp-login-title-color) max-[560px]:text-[30px]"
      >
        {{ t('auth.title') }}
      </h1>
      <p class="m-0 text-sm leading-[1.45] font-normal text-(--cp-login-description-color) [&:lang(zh)]:-ml-2">
        {{ t('auth.welcome') }}
      </p>
    </section>

    <div class="grid gap-3">
      <template v-if="!isKeyRealm">
        <div class="grid min-w-0 gap-2">
          <span id="admin-username-label" class="text-cp leading-[1.1] font-bold text-(--cp-login-label-color)">{{ t('auth.username') }}</span>
          <ZInput
            v-model="username"
            name="username"
            :aria-label="t('auth.username')"
            :placeholder="t('auth.usernamePlaceholder')"
            autocomplete="username"
          >
            <template #prefix>
              <Mail :size="17" />
            </template>
          </ZInput>
        </div>

        <div class="grid min-w-0 gap-2">
          <span class="text-cp leading-[1.1] font-bold text-(--cp-login-label-color)">{{ t('auth.password') }}</span>
          <ZInput
            v-model="password"
            name="password"
            :aria-label="t('auth.password')"
            :placeholder="t('auth.passwordPlaceholder')"
            show-password
            autocomplete="current-password"
          >
            <template #prefix>
              <KeyRound :size="17" />
            </template>
          </ZInput>
        </div>
      </template>

      <div v-else class="grid min-w-0 gap-2">
        <span id="client-api-key-label" class="text-cp leading-[1.1] font-bold text-(--cp-login-label-color)">{{ t('auth.apiKey') }}</span>
        <ZInput
          id="client-api-key"
          v-model="apiKey"
          name="apiKey"
          aria-labelledby="client-api-key-label"
          :placeholder="t('auth.apiKeyPlaceholder')"
          show-password
          autocomplete="off"
          autocapitalize="none"
          spellcheck="false"
        >
          <template #prefix>
            <KeyRound :size="17" />
          </template>
        </ZInput>
      </div>
      <div class="min-w-0">
        <ZButton
          type="primary"
          size="large"
          native-type="submit"
          class="login-submit"
          :loading="props.loading"
          :disabled="props.submitDisabled"
        >
          <span>{{ submitLabel }}</span>
        </ZButton>
      </div>
    </div>
  </ZCard>
</template>

<style scoped>
.login-form {
  --cp-color-bg-container: transparent;
  --cp-color-fill-quaternary: var(--cp-login-toggle-bg);
  --cp-color-fill-tertiary: var(--cp-login-input-bg);
  --cp-color-text: var(--cp-login-title-color);
  --cp-color-text-secondary: var(--cp-login-description-color);
  --cp-color-text-quaternary: var(--cp-login-placeholder-color);
  --cp-color-error-container: var(--cp-login-error-bg);
  --cp-color-error-border: transparent;
  --cp-color-error-text: var(--cp-login-error-text-color);
  --cp-color-error: var(--cp-login-error-icon-color);
  --cp-color-bg-container-disabled: var(--cp-login-disabled-bg);
  --cp-color-text-disabled: var(--cp-login-disabled-text-color);
  --cp-input-bg: var(--cp-login-input-bg);
  --cp-input-hover-bg: var(--cp-login-input-hover-bg);
  --cp-input-active-bg: var(--cp-login-input-active-bg);
  --cp-border-radius-sm: 6px;
  --cp-border-radius: 6px;
  --cp-box-shadow-tertiary: none;
  --cp-box-shadow: none;
  --cp-control-height: 43px;

  background:
    linear-gradient(
      118deg,
      var(--cp-login-panel-bg-start),
      var(--cp-login-panel-bg-middle) 56%,
      var(--cp-login-panel-bg-end)
    ),
    var(--cp-login-panel-bg-middle);
  box-shadow: 0 18px 38px -20px var(--cp-login-panel-shadow-color);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
}

.login-form-line {
  position: absolute;
  top: 0;
  left: 22px;
  width: calc(100% - 44px);
  height: 2px;
  background: linear-gradient(
    90deg,
    var(--cp-color-transparent),
    var(--cp-login-panel-line-color),
    var(--cp-color-transparent)
  );
  opacity: 0.42;
  pointer-events: none;
}

:global(html[data-theme='dark'] .login-form-line) {
  opacity: 0.3;
}

.login-logo {
  display: inline-flex;
  width: 42px;
  height: 42px;
  flex: 0 0 auto;
  align-items: center;
  justify-content: center;
  border-radius: 8px;
  color: var(--cp-login-logo-color);
  font-family: var(--font-mono);
  font-size: 12px;
  font-weight: 600;
  line-height: 1;
}

.login-submit {
  width: 100%;
  height: 44px;
  box-shadow: 0 14px 24px -18px var(--cp-login-button-shadow-color);
}

.login-submit:disabled {
  background: var(--cp-login-disabled-bg);
  box-shadow: none;
  transform: none;
}

@media (prefers-reduced-motion: reduce) {
  .login-submit {
    transition: none;
  }
}
</style>
