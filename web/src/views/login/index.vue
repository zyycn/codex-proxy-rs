<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { ArrowRight, Cat, KeyRound, Mail } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import { useAuthStore } from '@/stores/modules/auth'

const router = useRouter()
const authStore = useAuthStore()

const username = ref('')
const password = ref('')

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
    <div class="login-backdrop" aria-hidden="true">
      <svg class="login-flow-map" viewBox="0 0 1200 720" fill="none" preserveAspectRatio="none">
        <defs>
          <linearGradient id="login-flow-primary" x1="0" x2="1" y1="0" y2="0">
            <stop offset="0" stop-color="#2563eb" stop-opacity="0.02" />
            <stop offset="0.34" stop-color="#2563eb" stop-opacity="0.18" />
            <stop offset="0.68" stop-color="#0f9f9a" stop-opacity="0.28" />
            <stop offset="1" stop-color="#6d5dfc" stop-opacity="0.04" />
          </linearGradient>
          <linearGradient id="login-flow-muted" x1="0" x2="1" y1="0" y2="0">
            <stop offset="0" stop-color="#0f9f9a" stop-opacity="0.02" />
            <stop offset="0.38" stop-color="#0f9f9a" stop-opacity="0.16" />
            <stop offset="0.78" stop-color="#2563eb" stop-opacity="0.18" />
            <stop offset="1" stop-color="#2563eb" stop-opacity="0.02" />
          </linearGradient>
        </defs>
        <path
          class="login-flow-line login-flow-line-a"
          d="M-96 452 C96 338 260 332 430 410 C628 502 744 458 914 316 C1044 208 1132 190 1296 246"
          stroke="url(#login-flow-primary)"
          stroke-width="2"
        />
        <path
          class="login-flow-line login-flow-line-b"
          d="M-96 268 C86 346 252 386 438 294 C620 204 788 190 970 304 C1084 376 1176 386 1296 328"
          stroke="url(#login-flow-muted)"
          stroke-width="1.35"
        />
        <path
          class="login-flow-line login-flow-line-c"
          d="M-96 584 C104 514 260 494 438 558 C626 626 786 650 984 512 C1112 422 1190 420 1296 468"
          stroke="url(#login-flow-primary)"
          stroke-width="1.15"
        />
      </svg>

      <span class="login-runner login-runner-a" />
    </div>

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
        <p>安全转发 OpenAI / Codex 请求，账号、密钥、记录清晰可查</p>
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
            type="password"
            autocomplete="current-password"
          >
            <template #prefix>
              <KeyRound :size="17" />
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
  --login-glow-primary: #e6f4ff;
  --login-glow-normal: #e9fbf7;
  --login-bg-start: #f8fbff;
  --login-bg-mid: #ffffff;
  --login-bg-end: #eef4ff;
  --login-wash-line: #ffffffd6;
  --login-wash-a: #2563eb18;
  --login-wash-b: #0f9f9a18;
  --login-wash-c: #64748b10;
  --login-grid-y: #0e17260a;
  --login-grid-x: #0e172608;
  --login-runner-a: #2563ebcc;
  --login-runner-b: #0f9f9acc;
  --login-runner-shadow: #2563eb24;
  --login-form-top: #fffffff5;
  --login-form-bottom: #fffffff0;
  --login-form-edge-a: #ffffff;
  --login-form-edge-b: #dbeafe80;
  --login-form-edge-c: #c7efee66;
  --login-form-shadow: #0e17264d;
  --login-form-highlight-a: #93c5fd;
  --login-form-highlight-b: #a7dfdd;
  position: relative;
  isolation: isolate;
  min-height: 100dvh;
  overflow-x: hidden;
  overflow-y: auto;
  color: var(--cp-text-primary);
  background:
    radial-gradient(circle at 12% 20%, var(--login-glow-primary) 0, transparent 30%),
    radial-gradient(circle at 88% 24%, var(--login-glow-normal) 0, transparent 26%),
    linear-gradient(
      116deg,
      var(--login-bg-start) 0%,
      var(--login-bg-mid) 50%,
      var(--login-bg-end) 100%
    );
}

:global(html[data-theme='dark'] .login-page) {
  --login-glow-primary: #123153;
  --login-glow-normal: #0b3a38;
  --login-bg-start: #08101b;
  --login-bg-mid: #0b111c;
  --login-bg-end: #101a2a;
  --login-wash-line: #dbeafe12;
  --login-wash-a: #6ea8ff20;
  --login-wash-b: #2dd4bf1f;
  --login-wash-c: #8fa0b714;
  --login-grid-y: #e6edf70b;
  --login-grid-x: #e6edf708;
  --login-runner-a: #6ea8ffcc;
  --login-runner-b: #2dd4bfcc;
  --login-runner-shadow: #6ea8ff30;
  --login-form-top: #121b2af2;
  --login-form-bottom: #101827f0;
  --login-form-edge-a: #ffffff10;
  --login-form-edge-b: #6ea8ff42;
  --login-form-edge-c: #2dd4bf36;
  --login-form-shadow: #000000a3;
  --login-form-highlight-a: #6ea8ff;
  --login-form-highlight-b: #2dd4bf;
}

.login-backdrop {
  pointer-events: none;
  position: absolute;
  inset: 0;
  z-index: -1;
  overflow: hidden;
}

.login-backdrop::before {
  position: absolute;
  inset: -20%;
  content: '';
  background:
    linear-gradient(112deg, #ffffff00 22%, var(--login-wash-line) 50%, #ffffff00 74%),
    conic-gradient(
      from 145deg at 46% 48%,
      var(--login-wash-a),
      var(--login-wash-b),
      var(--login-wash-c),
      var(--login-wash-a)
    );
  filter: blur(42px);
  opacity: 0.58;
  animation: login-wash 20s ease-in-out infinite alternate;
}

.login-backdrop::after {
  position: absolute;
  inset: -1px;
  content: '';
  background-image:
    linear-gradient(var(--login-grid-y) 1px, transparent 1px),
    linear-gradient(90deg, var(--login-grid-x) 1px, transparent 1px);
  background-size: 58px 58px;
  mask-image: linear-gradient(90deg, transparent 0%, #000 14%, #000 78%, transparent 100%);
  opacity: 0.32;
  animation: login-grid-drift 28s linear infinite;
}

.login-flow-map {
  position: absolute;
  inset: -4% -12%;
  width: 124%;
  height: 112%;
  opacity: 0.68;
}

.login-flow-line {
  stroke-linecap: round;
  stroke-linejoin: round;
  stroke-dasharray: 14 30;
  animation: login-line-flow 22s linear infinite;
}

.login-flow-line-b {
  animation-direction: reverse;
}

.login-flow-line-c {
  stroke-dasharray: 7 24;
  opacity: 0.56;
}

.login-runner {
  position: absolute;
  width: 42px;
  height: 2px;
  border-radius: var(--cp-radius-circle);
  background: linear-gradient(
    90deg,
    transparent,
    var(--login-runner-a),
    var(--login-runner-b),
    transparent
  );
  box-shadow: 0 0 18px var(--login-runner-shadow);
  opacity: 0;
}

.login-runner-a {
  top: 46%;
  left: 10%;
  animation: login-runner-a 9s cubic-bezier(0.45, 0, 0.2, 1) infinite;
}

.login-stage {
  position: relative;
  z-index: 1;
  display: grid;
  width: min(calc(100% - 40px), 520px);
  min-height: 100dvh;
  margin: 0 auto;
  padding: clamp(24px, 5vh, 56px) 0;
  align-content: center;
  gap: clamp(26px, 4.8vh, 46px);
}

.login-brand {
  position: absolute;
  top: clamp(28px, 5vh, 56px);
  left: 0;
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

.login-copy {
  display: grid;
  gap: 13px;
}

.login-copy::before {
  width: 42px;
  height: 2px;
  content: '';
  border-radius: var(--cp-radius-circle);
  background: linear-gradient(90deg, #2563eb, #0f9f9a);
  opacity: 0.72;
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
  position: relative;
  display: grid;
  width: 100%;
  gap: 23px;
  padding: clamp(32px, 7vw, 42px);
  border: 1px solid transparent;
  background:
    linear-gradient(var(--login-form-top), var(--login-form-bottom)) padding-box,
    linear-gradient(
        135deg,
        var(--login-form-edge-a),
        var(--login-form-edge-b),
        var(--login-form-edge-c)
      )
      border-box;
  box-shadow:
    0 1px 0 var(--login-form-edge-a) inset,
    0 28px 64px -42px var(--login-form-shadow);
  backdrop-filter: blur(20px);
}

.login-form::before {
  position: absolute;
  top: 0;
  right: 34px;
  left: 34px;
  height: 1px;
  content: '';
  background: linear-gradient(
    90deg,
    transparent,
    var(--login-form-highlight-a),
    var(--login-form-highlight-b),
    transparent
  );
  opacity: 0.7;
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
  border: 1px solid var(--cp-danger-border);
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
  --cp-input-height-default: 46px;
}

.login-submit {
  width: 100%;
}

@keyframes login-wash {
  0% {
    transform: translate3d(-3%, 1%, 0) rotate(-3deg) scale(1);
  }

  100% {
    transform: translate3d(3%, -2%, 0) rotate(3deg) scale(1.045);
  }
}

@keyframes login-grid-drift {
  to {
    background-position: 58px 58px;
  }
}

@keyframes login-line-flow {
  to {
    stroke-dashoffset: -240;
  }
}

@keyframes login-runner-a {
  0% {
    opacity: 0;
    transform: translate3d(0, 0, 0) rotate(-8deg);
  }

  18%,
  72% {
    opacity: 0.72;
  }

  100% {
    opacity: 0;
    transform: translate3d(54vw, -18vh, 0) rotate(-17deg);
  }
}

@media (min-width: 720px) {
  .login-stage {
    width: min(calc(100% - 80px), 640px);
  }
}

@media (min-width: 1100px) {
  .login-stage {
    width: min(calc(100% - 112px), 1240px);
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
      radial-gradient(circle at 18% 12%, var(--login-glow-primary) 0, transparent 40%),
      linear-gradient(
        116deg,
        var(--login-bg-start) 0%,
        var(--login-bg-mid) 50%,
        var(--login-bg-end) 100%
      );
  }

  .login-backdrop::after {
    background-size: 42px 42px;
    opacity: 0.28;
  }

  .login-flow-map {
    inset: 8% -52%;
    width: 204%;
    height: 86%;
    opacity: 0.66;
  }

  .login-runner {
    display: none;
  }
}

@media (prefers-reduced-motion: reduce) {
  .login-backdrop::before,
  .login-backdrop::after,
  .login-flow-line,
  .login-runner {
    animation: none;
  }
}
</style>
