<script setup lang="ts">
import { ArrowRight, CircleAlert, Eye, EyeOff, KeyRound, Mail, Moon, Sun } from '@lucide/vue'
import { computed, shallowRef } from 'vue'

import AppBrandMark from '@/components/AppBrandMark.vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseMotionIcon from '@/components/base/BaseMotionIcon.vue'

type ThemeName = 'light' | 'dark'
type PasswordInputType = 'password' | 'text'

const props = defineProps<{
  error?: string | null
  loading: boolean
  submitDisabled: boolean
  effectiveTheme: ThemeName
}>()

const emit = defineEmits<{
  submit: []
  toggleTheme: [event: MouseEvent]
}>()

const username = defineModel<string>('username', { required: true })
const password = defineModel<string>('password', { required: true })
const isPasswordVisible = shallowRef(false)

const passwordType = computed<PasswordInputType>(() => (isPasswordVisible.value ? 'text' : 'password'))
const passwordToggleLabel = computed<string>(() => (isPasswordVisible.value ? '隐藏密码' : '显示密码'))
const submitLabel = computed<string>(() => (props.loading ? '正在进入...' : '进入控制台'))
const themeToggleLabel = computed<string>(() => (props.effectiveTheme === 'dark' ? '切换浅色模式' : '切换暗黑模式'))
const themeToggleClasses = computed<Record<string, boolean>>(() => ({
  'is-dark': props.effectiveTheme === 'dark',
}))

function togglePasswordVisible(): void {
  isPasswordVisible.value = !isPasswordVisible.value
}
</script>

<template>
  <BaseCard
    as="form"
    padding="none"
    class="login-form relative grid min-h-129.25 w-[min(440px,100%)] gap-2.5 rounded-lg px-7.5 pt-6.5 pb-6 max-[560px]:min-h-auto max-[560px]:gap-2.5 max-[560px]:p-5.5"
    @submit.prevent="emit('submit')"
  >
    <div class="login-form-line" />

    <header class="flex min-w-0 items-center justify-between gap-4.5 max-[560px]:gap-3.5">
      <div class="flex min-w-0 items-center gap-3">
        <BaseMotionIcon variant="brand" class="login-logo">
          <AppBrandMark class="block size-9.5 select-none" />
        </BaseMotionIcon>
        <span class="grid min-w-0 gap-1">
          <strong
            class="text-[17px] leading-[1.12] font-semibold text-(--cp-login-brand-title-color) max-[560px]:text-cp-xl"
          >
            Codex Proxy RS
          </strong>
          <span class="font-mono text-[10px] leading-[1.2] font-normal text-(--cp-login-brand-caption-color)">
            ADMIN REALM
          </span>
        </span>
      </div>

      <button
        class="login-theme-toggle"
        :class="themeToggleClasses"
        type="button"
        :aria-label="themeToggleLabel"
        :title="themeToggleLabel"
        @click="emit('toggleTheme', $event)"
      >
        <Sun :size="16" />
        <span class="login-theme-knob" />
        <Moon :size="16" />
      </button>
    </header>

    <section class="grid min-w-0 gap-1" aria-labelledby="login-title">
      <h1
        id="login-title"
        class="m-0 text-[34px] leading-[1.02] font-semibold text-(--cp-login-title-color) max-[560px]:text-[30px]"
      >
        控制台登录
      </h1>
      <p class="m-0 -ml-2 text-sm leading-[1.45] font-normal text-(--cp-login-description-color)">
        「 欢迎回来，登录以开始您的数据之旅 」
      </p>
    </section>

    <div class="grid gap-3">
      <div v-if="props.error" class="login-error" role="alert">
        <CircleAlert :size="16" />
        <p>{{ props.error }}</p>
      </div>

      <div class="grid min-w-0 gap-2">
        <span class="text-cp leading-[1.1] font-bold text-(--cp-login-label-color)">管理员账号</span>
        <BaseInput
          v-model="username"
          name="username"
          aria-label="管理员账号"
          placeholder="输入会话账号"
          autocomplete="username"
        >
          <template #prefix>
            <Mail :size="17" />
          </template>
        </BaseInput>
      </div>

      <div class="grid min-w-0 gap-2">
        <span class="text-cp leading-[1.1] font-bold text-(--cp-login-label-color)">访问密钥</span>
        <BaseInput
          v-model="password"
          name="password"
          aria-label="访问密钥"
          placeholder="输入会话密钥"
          :type="passwordType"
          autocomplete="current-password"
        >
          <template #prefix>
            <KeyRound :size="17" />
          </template>
          <template #suffix>
            <BaseIconButton
              variant="ghost"
              size="sm"
              class="login-password-toggle"
              :label="passwordToggleLabel"
              @mousedown.prevent
              @click="togglePasswordVisible"
            >
              <EyeOff v-if="isPasswordVisible" :size="16" />
              <Eye v-else :size="16" />
            </BaseIconButton>
          </template>
        </BaseInput>
      </div>

      <div class="min-w-0 mb-2">
        <BaseButton
          variant="primary"
          size="lg"
          type="submit"
          class="login-submit"
          :loading="props.loading"
          :disabled="props.submitDisabled"
        >
          <span>{{ submitLabel }}</span>
          <ArrowRight v-if="!props.loading" :size="18" />
        </BaseButton>
      </div>
    </div>
  </BaseCard>
</template>

<style scoped>
.login-form {
  --cp-color-bg-container: transparent;
  --cp-color-fill-quaternary: var(--cp-login-toggle-bg);
  --cp-color-fill-tertiary: var(--cp-login-input-bg);
  --cp-color-text: var(--cp-login-title-color);
  --cp-color-text-secondary: var(--cp-login-description-color);
  --cp-color-text-quaternary: var(--cp-login-placeholder-color);
  --cp-color-error-bg: var(--cp-login-error-bg);
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
  width: 38px;
  height: 38px;
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

.login-theme-toggle {
  position: relative;
  display: inline-grid;
  width: 66px;
  height: 32px;
  flex: 0 0 auto;
  grid-template-columns: 1fr 1fr;
  place-items: center;
  padding: 0;
  border: 0;
  border-radius: 20px;
  background: var(--cp-login-toggle-bg);
  color: var(--cp-login-toggle-moon-color);
  cursor: pointer;
  outline: none;
  transition:
    background 0.16s ease,
    color 0.16s ease;
}

@media (hover: hover) {
  .login-theme-toggle:hover {
    background: var(--cp-login-toggle-bg-hover);
  }
}

.login-theme-toggle:focus-visible {
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--cp-login-input-icon-color) 46%, transparent);
}

.login-theme-toggle > svg {
  position: relative;
  z-index: 1;
}

.login-theme-toggle > svg:first-child {
  color: var(--cp-login-toggle-sun-color);
}

.login-theme-toggle > svg:last-child {
  color: var(--cp-login-toggle-moon-color);
}

.login-theme-knob {
  position: absolute;
  top: 5px;
  left: 5.5px;
  width: 22px;
  height: 22px;
  border-radius: 50%;
  background: var(--cp-login-toggle-knob);
  box-shadow: 0 0 10px var(--cp-login-toggle-shadow-color);
  transition: transform 0.2s ease;
}

.login-theme-toggle.is-dark .login-theme-knob {
  transform: translateX(33px);
}

.login-error {
  display: flex;
  min-height: 38px;
  align-items: center;
  gap: 10px;
  border-radius: 6px;
  background: var(--cp-login-error-bg);
  padding: 0 12px;
  color: var(--cp-login-error-icon-color);
}

.login-error p {
  min-width: 0;
  margin: 0;
  color: var(--cp-login-error-text-color);
  font-size: 12px;
  font-weight: 600;
  line-height: 1.35;
}

.login-password-toggle {
  --cp-color-fill-quaternary: color-mix(in srgb, var(--cp-input-hover-bg) 62%, transparent);
  --cp-color-fill-tertiary: color-mix(in srgb, var(--cp-input-hover-bg) 88%, transparent);

  color: var(--cp-login-placeholder-color);
  border-radius: 6px;
}

.login-password-toggle:hover {
  color: var(--cp-login-title-color);
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
  .login-theme-knob,
  .login-submit,
  .login-theme-toggle {
    transition: none;
  }
}
</style>
