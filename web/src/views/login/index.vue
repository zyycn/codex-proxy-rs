<script setup lang="ts">
import { storeToRefs } from 'pinia'
import { computed, shallowRef } from 'vue'
import { useRouter } from 'vue-router'
import { ArrowRight, Cat, Eye, EyeOff, KeyRound, Mail, Moon, Sun } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import { useAuthStore } from '@/stores/modules/auth'
import { useUiStore } from '@/stores/modules/ui'

const router = useRouter()
const authStore = useAuthStore()
const uiStore = useUiStore()
const { effectiveTheme } = storeToRefs(uiStore)
const { toggleTheme } = uiStore

const username = shallowRef('')
const password = shallowRef('')
const passwordVisible = shallowRef(false)
const passwordType = computed(() => (passwordVisible.value ? 'text' : 'password'))
const passwordToggleLabel = computed(() => (passwordVisible.value ? '隐藏密码' : '显示密码'))
const themeToggleLabel = computed(() =>
  effectiveTheme.value === 'dark' ? '切换浅色模式' : '切换暗黑模式',
)

function togglePasswordVisible() {
  passwordVisible.value = !passwordVisible.value
}

async function handleSubmit() {
  if (!username.value.trim() || !password.value.trim()) {
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
  <main class="login-page">
    <div class="login-page-wash" aria-hidden="true" />

    <BaseButton
      icon-only
      variant="ghost"
      size="default"
      class="login-theme-toggle"
      :label="themeToggleLabel"
      @click="toggleTheme($event)"
    >
      <Sun v-if="effectiveTheme === 'dark'" :size="19" />
      <Moon v-else :size="19" />
    </BaseButton>

    <section class="login-stage" aria-label="Codex Proxy RS 登录">
      <header class="login-brand">
        <span class="login-logo">
          <Cat :size="27" stroke-width="2" />
        </span>
        <span class="login-brand-text">
          <strong>Codex Proxy RS</strong>
          <span>轻量模型网关</span>
        </span>
      </header>

      <section class="login-copy" aria-labelledby="login-title">
        <h1 id="login-title">更轻的模型网关</h1>
        <p>安全的转发 OpenAI / Codex 请求， 风控指纹对齐，链路轻量安全</p>
        <div class="login-scope" aria-label="管理范围">
          <span>轻量部署</span>
          <span>安全转发</span>
          <span>高效路由</span>
        </div>
      </section>

      <BaseCard
        as="form"
        :padded="false"
        variant="elevated"
        class="login-form"
        @submit.prevent="handleSubmit"
      >
        <header class="login-form-header">
          <h2>登录控制台</h2>
          <p>使用管理员账号继续管理网关。</p>
        </header>

        <div v-if="authStore.error" class="login-error">
          <p>
            {{ authStore.error }}
          </p>
        </div>

        <label class="login-field-group">
          <span>管理员账号</span>
          <BaseInput
            v-model="username"
            class="login-field"
            placeholder="请输入用户名"
            autocomplete="username"
          >
            <template #prefix>
              <Mail :size="17" />
            </template>
          </BaseInput>
        </label>

        <label class="login-field-group">
          <span>访问密钥</span>
          <BaseInput
            v-model="password"
            class="login-field"
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
                <EyeOff v-if="passwordVisible" :size="16" />
                <Eye v-else :size="16" />
              </BaseButton>
            </template>
          </BaseInput>
        </label>

        <BaseButton
          size="lg"
          type="submit"
          class="login-submit"
          :loading="authStore.loading"
          :disabled="authStore.loading || !username.trim() || !password.trim()"
        >
          <span>{{ authStore.loading ? '登录中...' : '登录' }}</span>
          <ArrowRight v-if="!authStore.loading" :size="18" />
        </BaseButton>
      </BaseCard>
    </section>
  </main>
</template>

<style scoped>
.login-page {
  --login-bg-a: #f7fafc;
  --login-bg-b: #eef6ff;
  --login-bg-c: #f8fbf5;
  --login-grid-line: #0e17260a;
  --login-rail-line: #2563eb0d;
  --login-wash-line: #ffffff8f;
  --login-depth-x: 0px;
  --login-depth-y: 0px;
  --login-form-glass-a: #ffffff72;
  --login-form-glass-mid: #f6fbff48;
  --login-form-glass-b: #eef7f22e;
  --login-form-sheen: #ffffff36;
  --login-form-shadow: #0e17264a;
  --login-form-glow: #5eead426;
  --login-form-highlight-a: #60a5fa2e;
  --login-form-highlight-b: #2dd4bf24;
  --login-control-height: 46px;
  position: relative;
  isolation: isolate;
  height: 100dvh;
  overflow-x: hidden;
  overflow-y: hidden;
  color: var(--cp-text-primary);
  background:
    linear-gradient(90deg, var(--login-grid-line) 1px, transparent 1px),
    linear-gradient(var(--login-grid-line) 1px, transparent 1px),
    linear-gradient(135deg, var(--login-bg-a) 0%, var(--login-bg-b) 52%, var(--login-bg-c) 100%);
  background-size:
    72px 72px,
    72px 72px,
    auto;
}

:global(html[data-theme='dark'] .login-page) {
  --login-bg-a: #08101b;
  --login-bg-b: #101827;
  --login-bg-c: #0d1819;
  --login-grid-line: #e6edf70a;
  --login-rail-line: #6ea8ff12;
  --login-wash-line: #ffffff12;
  --login-form-glass-a: #31466536;
  --login-form-glass-mid: #14274328;
  --login-form-glass-b: #06132634;
  --login-form-sheen: #ffffff10;
  --login-form-shadow: #00000078;
  --login-form-glow: #2dd4bf18;
  --login-form-highlight-a: #6ea8ff20;
  --login-form-highlight-b: #2dd4bf18;
}

.login-page::before {
  position: absolute;
  inset: 0;
  z-index: -2;
  content: '';
  background:
    linear-gradient(112deg, transparent 18%, var(--login-wash-line) 44%, transparent 64%),
    repeating-linear-gradient(135deg, transparent 0 30px, var(--login-rail-line) 31px 32px);
  mask-image: linear-gradient(90deg, transparent 0%, #000 18%, #000 88%, transparent 100%);
}

.login-page-wash {
  position: absolute;
  inset: -18%;
  z-index: -1;
  pointer-events: none;
  background:
    conic-gradient(from 210deg at 48% 42%, #2563eb18, #0f9f9a16, #f59e0b12, #2563eb18),
    linear-gradient(90deg, transparent, #ffffff54, transparent);
  filter: blur(38px);
  opacity: 0.66;
  transform: rotate(-2deg);
}

:global(html[data-theme='dark'] .login-page-wash) {
  background:
    conic-gradient(from 210deg at 48% 42%, #6ea8ff22, #2dd4bf1a, #fbbf2412, #6ea8ff22),
    linear-gradient(90deg, transparent, #ffffff0c, transparent);
  opacity: 0.72;
}

.login-stage {
  position: relative;
  z-index: 1;
  box-sizing: border-box;
  display: grid;
  width: min(calc(100% - 40px), 520px);
  height: 100dvh;
  margin: 0 auto;
  padding: clamp(24px, 5vh, 56px) 0;
  align-content: center;
  gap: clamp(26px, 4.8vh, 46px);
}

.login-brand {
  position: fixed;
  top: clamp(22px, 4vh, 40px);
  left: clamp(20px, 4vw, 56px);
  z-index: 2;
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 13px;
}

.login-logo {
  display: inline-flex;
  width: 46px;
  height: 46px;
  flex: 0 0 auto;
  align-items: center;
  justify-content: center;
  border-radius: var(--cp-icon-button-radius);
  background: var(--cp-bg-dark);
  color: var(--cp-white);
  box-shadow: 0 14px 28px -22px #0e1726;
}

.login-brand-text {
  display: grid;
  gap: 7px;
  min-width: 0;
}

.login-brand-text strong {
  color: var(--cp-text-primary);
  font-size: 17px;
  font-weight: 760;
  line-height: 1.1;
}

.login-brand-text span {
  color: var(--cp-text-secondary);
  font-size: 11px;
  font-weight: 650;
  line-height: 1.15;
}

.login-theme-toggle {
  position: fixed;
  top: clamp(24px, 4vh, 42px);
  right: clamp(20px, 4vw, 56px);
  z-index: 2;
  color: var(--cp-text-secondary);
  background: color-mix(in srgb, var(--cp-bg-surface) 54%, transparent);
  box-shadow: 0 18px 34px -28px #0e1726;
  backdrop-filter: blur(10px) saturate(1.2);
  -webkit-backdrop-filter: blur(10px) saturate(1.2);
}

.login-theme-toggle:hover {
  color: var(--cp-text-primary);
  background: color-mix(in srgb, var(--cp-bg-surface) 72%, transparent);
}

.login-copy {
  display: grid;
  gap: 13px;
}

.login-copy::before {
  width: 20px;
  height: 2px;
  content: '';
  border-radius: var(--cp-radius-circle);
  background: linear-gradient(90deg, #2563eb, #0f9f9a);
  opacity: 0.72;
  position: relative;
  left: 72px;
  top: 2px;
}

.login-copy h1 {
  max-width: 12em;
  margin: 0;
  color: var(--cp-text-primary);
  font-size: clamp(34px, 9vw, 48px);
  font-weight: 760;
  line-height: 1.04;
  letter-spacing: 0;
}

.login-copy p {
  max-width: 40rem;
  margin: 0;
  color: var(--cp-text-secondary);
  font-size: clamp(13px, 3.5vw, 15px);
  font-weight: 580;
  line-height: 1.55;
}

.login-scope {
  display: inline-flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0;
  padding-top: 3px;
}

.login-scope span {
  position: relative;
  display: inline-block;
  padding: 0 13px;
  color: var(--cp-text-secondary);
  font-size: 12px;
  font-weight: 700;
  line-height: 1;
}

.login-scope span:first-child {
  padding-left: 0;
}

.login-scope span + span::before {
  position: absolute;
  top: 50%;
  left: 0;
  width: 1px;
  height: 10px;
  content: '';
  background: var(--cp-default-border-hover);
  transform: translateY(-50%);
}

.login-form {
  --cp-bg-surface: transparent;
  position: relative;
  isolation: isolate;
  display: grid;
  width: 100%;
  gap: 23px;
  padding: clamp(32px, 7vw, 42px);
  background-color: transparent;
  background:
    radial-gradient(circle at 16% 0%, var(--login-form-sheen) 0%, transparent 34%),
    radial-gradient(circle at 96% 8%, var(--login-form-highlight-b) 0%, transparent 36%),
    linear-gradient(
      145deg,
      var(--login-form-glass-a),
      var(--login-form-glass-mid) 48%,
      var(--login-form-glass-b)
    );
  box-shadow:
    0 44px 96px -48px var(--login-form-shadow),
    0 18px 42px -36px var(--login-form-glow),
    0 24px 64px -58px var(--login-form-highlight-a) inset,
    0 -18px 42px -40px var(--login-form-highlight-b) inset;
  backdrop-filter: blur(9px) saturate(1.45);
  -webkit-backdrop-filter: blur(9px) saturate(1.45);
}

.login-form::before {
  position: absolute;
  inset: 0;
  z-index: 0;
  pointer-events: none;
  content: '';
  background:
    linear-gradient(120deg, var(--login-form-sheen), transparent 32%),
    radial-gradient(circle at 72% 0%, var(--login-form-highlight-a), transparent 36%);
  opacity: 0.12;
}

.login-form > * {
  position: relative;
  z-index: 1;
}

.login-form-header {
  display: grid;
  gap: 10px;
}

.login-form-header h2 {
  margin: 0;
  color: var(--cp-text-primary);
  font-size: clamp(31px, 7vw, 36px);
  font-weight: 800;
  line-height: 1.12;
  letter-spacing: 0;
}

.login-form-header p {
  margin: 0;
  color: var(--cp-text-secondary);
  font-size: 14px;
  font-weight: 560;
  line-height: 1.55;
}

.login-error {
  border-radius: var(--cp-input-radius-base);
  background: var(--cp-danger-bg);
  padding: 12px 16px;
}

.login-error p {
  margin: 0;
  color: var(--cp-danger-text);
  font-size: 13px;
  font-weight: 650;
}

.login-field-group {
  display: grid;
  gap: 8px;
}

.login-field-group > span {
  color: var(--cp-text-primary);
  font-size: 12px;
  font-weight: 720;
  line-height: 1.1;
}

.login-field {
  --cp-input-height-default: var(--login-control-height);
}

.login-password-toggle {
  flex: 0 0 auto;
  color: var(--cp-text-muted);
}

.login-password-toggle:hover {
  color: var(--cp-text-primary);
}

.login-submit {
  width: 100%;
}

@media (min-width: 720px) {
  .login-stage {
    width: min(calc(100% - 80px), 640px);
  }
}

@media (min-width: 1100px) {
  .login-stage {
    width: min(calc(100% - 112px), 1280px);
    grid-template-columns: minmax(0, 1fr) minmax(452px, 500px);
    grid-template-areas: 'copy form';
    column-gap: clamp(82px, 9vw, 148px);
    align-content: center;
  }

  .login-copy {
    grid-area: copy;
    align-self: center;
  }

  .login-form {
    grid-area: form;
    align-self: center;
  }

  .login-copy h1 {
    max-width: 10.5em;
    font-size: clamp(44px, 3.4vw, 54px);
  }
}

@media (max-width: 520px) {
  .login-page {
    background:
      linear-gradient(90deg, var(--login-grid-line) 1px, transparent 1px),
      linear-gradient(var(--login-grid-line) 1px, transparent 1px),
      linear-gradient(135deg, var(--login-bg-a) 0%, var(--login-bg-b) 52%, var(--login-bg-c) 100%);
    background-size:
      48px 48px,
      48px 48px,
      auto;
  }

  .login-stage {
    gap: clamp(18px, 3.4vh, 28px);
    padding: clamp(18px, 3.6vh, 32px) 0;
  }

  .login-brand {
    top: 18px;
    left: 20px;
  }

  .login-theme-toggle {
    top: 18px;
    right: 20px;
  }
}

@media (prefers-reduced-motion: reduce) {
  .login-page-wash {
    transform: none;
  }
}
</style>
