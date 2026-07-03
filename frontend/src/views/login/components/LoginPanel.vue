<script setup lang="ts">
import { ArrowRight, Cat, CircleAlert, Eye, EyeOff, KeyRound, Mail, Moon, Sun } from '@lucide/vue'
import { computed, shallowRef } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseInput from '@/components/base/BaseInput.vue'

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

const passwordType = computed<PasswordInputType>(() =>
  isPasswordVisible.value ? 'text' : 'password',
)
const passwordToggleLabel = computed<string>(() =>
  isPasswordVisible.value ? '隐藏密码' : '显示密码',
)
const submitLabel = computed<string>(() => (props.loading ? '正在进入...' : '进入控制台'))
const themeToggleLabel = computed<string>(() =>
  props.effectiveTheme === 'dark' ? '切换浅色模式' : '切换暗黑模式',
)
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
    :padded="false"
    variant="elevated"
    class="login-form relative grid min-h-129.25 w-[min(450px,100%)] gap-4 rounded-lg px-7.5 pt-6.5 pb-6 max-[560px]:min-h-auto max-[560px]:gap-4 max-[560px]:p-5.5"
    @submit.prevent="emit('submit')"
  >
    <div class="login-form-line" aria-hidden="true" />

    <header class="flex min-w-0 items-center justify-between gap-4.5 max-[560px]:gap-3.5">
      <div class="flex min-w-0 items-center gap-3">
        <span class="login-logo">
          <Cat :size="20" :stroke-width="2.1" />
        </span>
        <span class="grid min-w-0 gap-1">
          <strong
            class="text-[17px] leading-[1.12] font-semibold text-(--login-brand-title) max-[560px]:text-[15px]"
          >
            Codex Proxy RS
          </strong>
          <span
            class="font-mono text-[10px] leading-[1.2] font-normal text-(--login-brand-caption)"
          >
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
        <span class="login-theme-knob" aria-hidden="true" />
        <Moon :size="16" />
      </button>
    </header>

    <section class="grid min-w-0 gap-2.5" aria-labelledby="login-title">
      <h1
        id="login-title"
        class="m-0 text-[34px] leading-[1.02] font-semibold text-(--login-title) max-[560px]:text-[30px]"
      >
        控制台登录
      </h1>
      <p class="m-0 -ml-2 text-sm leading-[1.45] font-normal text-(--login-description)">
        「 欢迎回来，登录以开始您的数据之旅。 」
      </p>
    </section>

    <div class="grid gap-3">
      <div v-if="props.error" class="login-error" role="alert">
        <CircleAlert :size="16" />
        <p>{{ props.error }}</p>
      </div>

      <label class="grid min-w-0 gap-2">
        <span class="text-[13px] leading-[1.1] font-bold text-(--login-label)">管理员账号</span>
        <BaseInput v-model="username" placeholder="admin@example.com" autocomplete="username">
          <template #prefix>
            <Mail :size="17" />
          </template>
        </BaseInput>
      </label>

      <label class="grid min-w-0 gap-2">
        <span class="text-[13px] leading-[1.1] font-bold text-(--login-label)">访问密钥</span>
        <BaseInput
          v-model="password"
          placeholder="输入会话密钥"
          :type="passwordType"
          autocomplete="current-password"
        >
          <template #prefix>
            <KeyRound :size="17" />
          </template>
          <template #suffix>
            <BaseButton
              icon-only
              variant="ghost"
              size="sm"
              class="login-password-toggle"
              :label="passwordToggleLabel"
              @mousedown.prevent
              @click="togglePasswordVisible"
            >
              <EyeOff v-if="isPasswordVisible" :size="16" />
              <Eye v-else :size="16" />
            </BaseButton>
          </template>
        </BaseInput>
      </label>

      <div class="min-w-0 mb-2">
        <BaseButton
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
  --cp-bg-surface: transparent;
  --cp-bg-subtle: var(--login-toggle-bg);
  --cp-bg-muted: var(--login-input-bg);
  --cp-text-primary: var(--login-title);
  --cp-text-secondary: var(--login-description);
  --cp-text-muted: var(--login-placeholder);
  --cp-info: var(--login-button);
  --cp-info-hover: var(--login-button-hover);
  --cp-info-pressed: var(--login-button-active);
  --cp-info-on: var(--cp-white);
  --cp-info-border: var(--login-input-icon);
  --cp-danger-bg: var(--login-error-bg);
  --cp-danger-border: transparent;
  --cp-danger-text: var(--login-error-text);
  --cp-danger: var(--login-error-icon);
  --cp-disabled-bg: var(--login-disabled-bg);
  --cp-disabled-text: var(--login-disabled-text);
  --cp-disabled-icon: var(--login-disabled-text);
  --cp-button-radius-base: 6px;
  --cp-icon-button-radius: 6px;
  --cp-shadow-control: none;
  --cp-shadow-popover: none;
  --cp-input-height-default: 43px;

  background:
    linear-gradient(
      118deg,
      var(--login-form-bg-a),
      var(--login-form-bg-b) 56%,
      var(--login-form-bg-c)
    ),
    var(--login-form-bg-b);
  box-shadow: 0 18px 38px -20px var(--login-form-shadow);
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
    var(--cp-transparent),
    var(--login-form-line),
    var(--cp-transparent)
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
  background: var(--login-logo-bg);
  color: var(--login-logo-text);
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
  background: var(--login-toggle-bg);
  color: var(--login-toggle-moon);
  cursor: pointer;
  outline: none;
  transition:
    background 0.16s ease,
    color 0.16s ease;
}

.login-theme-toggle:focus-visible {
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--login-input-icon) 46%, transparent);
}

.login-theme-toggle > svg {
  position: relative;
  z-index: 1;
}

.login-theme-toggle > svg:first-child {
  color: var(--login-toggle-sun);
}

.login-theme-toggle > svg:last-child {
  color: var(--login-toggle-moon);
}

.login-theme-knob {
  position: absolute;
  top: 4px;
  left: 4px;
  width: 24px;
  height: 24px;
  border-radius: 50%;
  background: var(--login-toggle-knob);
  box-shadow: 0 0 10px var(--login-toggle-shadow);
  transition: transform 0.2s ease;
}

.login-theme-toggle.is-dark .login-theme-knob {
  transform: translateX(34px);
}

.login-error {
  display: flex;
  min-height: 38px;
  align-items: center;
  gap: 10px;
  border-radius: 6px;
  background: var(--login-error-bg);
  padding: 0 12px;
  color: var(--login-error-icon);
}

.login-error p {
  min-width: 0;
  margin: 0;
  color: var(--login-error-text);
  font-size: 12px;
  font-weight: 600;
  line-height: 1.35;
}

.login-password-toggle {
  --cp-bg-subtle: color-mix(in srgb, var(--cp-input-context-bg-hover) 62%, transparent);
  --cp-bg-muted: color-mix(in srgb, var(--cp-input-context-bg-hover) 88%, transparent);

  color: var(--login-placeholder);
  border-radius: 6px;
}

.login-password-toggle:hover {
  color: var(--login-title);
}

.login-submit {
  width: 100%;
  height: 44px;
  box-shadow: 0 14px 24px -18px var(--login-button-shadow);
}

.login-submit:disabled {
  background: var(--login-disabled-bg);
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
