<script setup lang="ts">
import { storeToRefs } from 'pinia'
import { computed, onMounted, onUnmounted, ref, shallowRef } from 'vue'
import { useRouter } from 'vue-router'
import { ArrowRight, Cat, Eye, EyeOff, KeyRound, Mail, Moon, Sun } from '@lucide/vue'
import { gsap } from 'gsap'

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
const canSubmit = computed(() => !!username.value.trim() && !!password.value.trim())
const submitDisabled = computed(() => authStore.loading || !canSubmit.value)
const themeToggleLabel = computed(() =>
  effectiveTheme.value === 'dark' ? '切换浅色模式' : '切换暗黑模式',
)

function togglePasswordVisible() {
  passwordVisible.value = !passwordVisible.value
}

async function handleSubmit() {
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

// ── GSAP ──────────────────────────────────────────────────────────────────────

const stageRef = ref<HTMLElement | null>(null)
let ctx: gsap.Context | undefined

onMounted(() => {
  if (!stageRef.value) return

  ctx = gsap.context(() => {
    const tl = gsap.timeline({ defaults: { ease: 'power3.out' } })

    // Brand logo pop-in
    tl.from('.login-brand', { autoAlpha: 0, y: -14, duration: 0.55 })
    tl.from('.login-theme-toggle', { autoAlpha: 0, y: -14, duration: 0.4 }, '<0.08')

    // Copy block cascades in from the left
    tl.from(
      '.login-copy::before',
      { scaleX: 0, transformOrigin: 'left center', duration: 0.5 },
      '-=0.1',
    )
    tl.from('.login-copy-headline', { autoAlpha: 0, x: -28, duration: 0.56 }, '-=0.3')
    tl.from('.login-copy-lead', { autoAlpha: 0, x: -20, duration: 0.5 }, '-=0.32')
    tl.from(
      '.login-scope span',
      {
        autoAlpha: 0,
        y: 10,
        duration: 0.38,
        stagger: 0.08,
      },
      '-=0.26',
    )

    // Form card rises from below
    tl.from(
      '.login-form',
      {
        autoAlpha: 0,
        y: 36,
        duration: 0.62,
        ease: 'power2.out',
      },
      '-=0.42',
    )

    // Form internals cascade. Keep submit timing separate so the disabled
    // state settles before the call-to-action appears.
    tl.from(
      '.login-form-header, .login-error, .login-field-group',
      {
        autoAlpha: 0,
        y: 14,
        duration: 0.4,
        stagger: 0.07,
        clearProps: 'all',
      },
      '-=0.38',
    )
    tl.from(
      '.login-submit-slot',
      {
        autoAlpha: 0,
        y: 10,
        duration: 0.34,
        clearProps: 'all',
      },
      '-=0.12',
    )
    tl.from(
      '.login-form-footer',
      {
        autoAlpha: 0,
        y: 8,
        duration: 0.28,
        clearProps: 'all',
      },
      '-=0.16',
    )

    // Floating orbs drift continuously
    gsap.to('.login-orb-a', {
      y: -24,
      x: 14,
      rotation: 8,
      duration: 7,
      repeat: -1,
      yoyo: true,
      ease: 'sine.inOut',
    })
    gsap.to('.login-orb-b', {
      y: 18,
      x: -18,
      rotation: -6,
      duration: 9,
      repeat: -1,
      yoyo: true,
      ease: 'sine.inOut',
      delay: 1.4,
    })
    gsap.to('.login-orb-c', {
      y: -16,
      x: 10,
      rotation: 5,
      duration: 11,
      repeat: -1,
      yoyo: true,
      ease: 'sine.inOut',
      delay: 2.8,
    })
  }, stageRef.value)
})

onUnmounted(() => {
  ctx?.revert()
})
</script>

<template>
  <main class="login-page">
    <!-- Decorative background wash -->
    <div class="login-page-wash" aria-hidden="true" />

    <!-- Floating orbs -->
    <div class="login-orbs" aria-hidden="true">
      <div class="login-orb login-orb-a" />
      <div class="login-orb login-orb-b" />
      <div class="login-orb login-orb-c" />
    </div>

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

    <section ref="stageRef" class="login-stage" aria-label="Codex Proxy RS 登录">
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
        <h1 id="login-title" class="login-copy-headline">更轻的模型网关</h1>
        <p class="login-copy-lead">安全的转发 OpenAI / Codex 请求，风控指纹对齐，链路轻量安全</p>
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
        <!-- Card inner glow layer -->
        <div class="login-form-glow-layer" aria-hidden="true" />

        <header class="login-form-header">
          <div>
            <h2>登录控制台</h2>
            <p>使用管理员账号继续管理网关。</p>
          </div>
        </header>

        <div v-if="authStore.error" class="login-error">
          <p>{{ authStore.error }}</p>
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

        <div class="login-submit-slot">
          <BaseButton
            size="lg"
            type="submit"
            class="login-submit"
            :loading="authStore.loading"
            :disabled="submitDisabled"
          >
            <span>{{ authStore.loading ? '登录中...' : '登录' }}</span>
            <ArrowRight v-if="!authStore.loading" :size="18" />
          </BaseButton>
        </div>

        <footer class="login-form-footer">
          <span>Codex Proxy RS</span>
          <span>轻量安全</span>
        </footer>
      </BaseCard>
    </section>
  </main>
</template>

<style scoped>
/* ── Design tokens (page-scoped overrides) ─────────────────────────────────── */
.login-page {
  --login-bg-a: #dfe7f0;
  --login-bg-b: #cbd9e9;
  --login-bg-c: #dce8e1;
  --login-grid-line: #0e172612;
  --login-rail-line: #2563eb16;
  --login-wash-line: #ffffff40;
  --login-form-glass-a: #ffffff8c;
  --login-form-glass-mid: #f4f8ff76;
  --login-form-glass-b: #e5f3ed68;
  --login-form-sheen: #ffffff92;
  --login-form-shadow: #0e172658;
  --login-form-glow: #5eead426;
  --login-form-highlight-a: #60a5fa2e;
  --login-form-highlight-b: #2dd4bf24;
  --login-input-bg: #edf2f8;
  --login-input-bg-hover: #f3f7fb;
  --login-control-height: 46px;
  --login-orb-a: #3b82f6;
  --login-orb-b: #0f9f9a;
  --login-orb-c: #a78bfa;

  position: relative;
  isolation: isolate;
  height: 100dvh;
  overflow: hidden;
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
  --login-bg-a: #07101b;
  --login-bg-b: #0e1626;
  --login-bg-c: #0c1719;
  --login-grid-line: #e6edf70a;
  --login-rail-line: #6ea8ff12;
  --login-wash-line: #ffffff12;
  --login-form-glass-a: #31466538;
  --login-form-glass-mid: #14274330;
  --login-form-glass-b: #06132638;
  --login-form-sheen: #ffffff12;
  --login-form-shadow: #00000080;
  --login-form-glow: #2dd4bf18;
  --login-form-highlight-a: #6ea8ff20;
  --login-form-highlight-b: #2dd4bf18;
  --login-input-bg: #182437;
  --login-input-bg-hover: #1d2d43;
  --login-orb-a: #3b82f6;
  --login-orb-b: #2dd4bf;
  --login-orb-c: #a78bfa;
}

/* ── Page wash ─────────────────────────────────────────────────────────────── */
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
  filter: blur(42px);
  opacity: 0.32;
  transform: rotate(-2deg);
}

:global(html[data-theme='dark'] .login-page-wash) {
  background:
    conic-gradient(from 210deg at 48% 42%, #6ea8ff22, #2dd4bf1a, #fbbf2412, #6ea8ff22),
    linear-gradient(90deg, transparent, #ffffff0c, transparent);
  opacity: 0.72;
}

/* ── Floating orbs ─────────────────────────────────────────────────────────── */
.login-orbs {
  position: absolute;
  inset: 0;
  z-index: 0;
  pointer-events: none;
  overflow: hidden;
}

.login-orb {
  position: absolute;
  border-radius: 50%;
  filter: blur(62px);
  opacity: 0.14;
  will-change: transform;
}

:global(html[data-theme='dark'] .login-orb) {
  opacity: 0.2;
}

.login-orb-a {
  width: 460px;
  height: 460px;
  background: var(--login-orb-a);
  top: -12%;
  right: -6%;
}

.login-orb-b {
  width: 360px;
  height: 360px;
  background: var(--login-orb-b);
  bottom: -8%;
  left: 4%;
}

.login-orb-c {
  width: 280px;
  height: 280px;
  background: var(--login-orb-c);
  top: 42%;
  left: 34%;
}

/* ── Theme toggle ──────────────────────────────────────────────────────────── */
.login-theme-toggle {
  position: fixed;
  top: clamp(24px, 4vh, 42px);
  right: clamp(20px, 4vw, 56px);
  z-index: 10;
  color: var(--cp-text-secondary);
  background: color-mix(in srgb, var(--cp-bg-surface) 60%, transparent);
  box-shadow: 0 18px 34px -28px #0e1726;
  backdrop-filter: blur(12px) saturate(1.3);
  -webkit-backdrop-filter: blur(12px) saturate(1.3);
}

.login-theme-toggle:hover {
  color: var(--cp-text-primary);
  background: color-mix(in srgb, var(--cp-bg-surface) 78%, transparent);
}

/* ── Stage layout ──────────────────────────────────────────────────────────── */
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

/* ── Brand ─────────────────────────────────────────────────────────────────── */
.login-brand {
  position: fixed;
  top: clamp(22px, 4vh, 40px);
  left: clamp(20px, 4vw, 56px);
  z-index: 10;
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
  box-shadow:
    0 14px 28px -22px #0e1726,
    inset 0 1px 0 #ffffff20;
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

/* ── Copy block ────────────────────────────────────────────────────────────── */
.login-copy {
  display: grid;
  gap: 14px;
}

.login-copy-headline::before {
  display: block;
  width: 24px;
  height: 2.5px;
  margin-bottom: 12px;
  content: '';
  border-radius: var(--cp-radius-circle);
  background: linear-gradient(90deg, #2563eb, #0f9f9a);
  opacity: 0.8;
  position: relative;
  left: 75px;
}

.login-copy-headline {
  margin: 0;
  color: var(--cp-text-primary);
  font-size: clamp(36px, 9vw, 52px);
  font-weight: 800;
  line-height: 1.06;
  letter-spacing: -0.01em;
  white-space: nowrap;
}

.login-copy-lead {
  max-width: 38rem;
  margin: 0;
  color: var(--cp-text-secondary);
  font-size: clamp(13px, 3.5vw, 15px);
  font-weight: 560;
  line-height: 1.6;
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

/* ── Form card ─────────────────────────────────────────────────────────────── */
.login-form {
  --cp-bg-surface: transparent;
  --cp-input-current-bg: var(--login-input-bg);
  --cp-input-current-bg-hover: var(--login-input-bg-hover);
  position: relative;
  isolation: isolate;
  display: grid;
  width: 100%;
  gap: 22px;
  padding: clamp(28px, 6vw, 40px);
  background-color: transparent;
  background:
    radial-gradient(circle at 14% 0%, var(--login-form-sheen) 0%, transparent 36%),
    radial-gradient(circle at 92% 10%, var(--login-form-highlight-b) 0%, transparent 38%),
    linear-gradient(
      148deg,
      var(--login-form-glass-a),
      var(--login-form-glass-mid) 50%,
      var(--login-form-glass-b)
    );
  box-shadow:
    0 30px 72px -34px var(--login-form-shadow),
    0 14px 34px -26px var(--login-form-glow),
    0 28px 64px -58px var(--login-form-highlight-a) inset,
    0 -20px 42px -40px var(--login-form-highlight-b) inset;
  backdrop-filter: blur(30px) saturate(1.55);
  -webkit-backdrop-filter: blur(30px) saturate(1.55);
}

.login-form-glow-layer {
  position: absolute;
  inset: 0;
  z-index: 0;
  pointer-events: none;
  border-radius: inherit;
  background:
    linear-gradient(118deg, var(--login-form-sheen), transparent 34%),
    radial-gradient(circle at 74% 2%, var(--login-form-highlight-a), transparent 38%);
  opacity: 0.18;
}

.login-form > * {
  position: relative;
  z-index: 1;
}

/* ── Form header ───────────────────────────────────────────────────────────── */
.login-form-header {
  display: flex;
  align-items: flex-start;
}

.login-form-header > div {
  display: grid;
  gap: 8px;
  min-width: 0;
}

.login-form-header h2 {
  margin: 0;
  color: var(--cp-text-primary);
  font-size: clamp(26px, 6vw, 32px);
  font-weight: 800;
  line-height: 1.1;
  letter-spacing: -0.01em;
}

.login-form-header p {
  margin: 0;
  color: var(--cp-text-secondary);
  font-size: 13px;
  font-weight: 560;
  line-height: 1.5;
}

/* ── Error banner ──────────────────────────────────────────────────────────── */
.login-error {
  border-radius: var(--cp-input-radius-base);
  background: var(--cp-danger-bg);
  padding: 12px 16px;
  border: 1px solid var(--cp-danger-border);
}

.login-error p {
  margin: 0;
  color: var(--cp-danger-text);
  font-size: 13px;
  font-weight: 650;
}

/* ── Field groups ──────────────────────────────────────────────────────────── */
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

/* ── Submit button ─────────────────────────────────────────────────────────── */
.login-submit-slot {
  min-width: 0;
}

.login-submit {
  width: 100%;
  background: linear-gradient(
    135deg,
    var(--cp-info) 0%,
    color-mix(in srgb, var(--cp-info) 70%, #0f9f9a) 100%
  );
  box-shadow:
    0 8px 24px -12px color-mix(in srgb, var(--cp-info) 60%, transparent),
    inset 0 1px 0 #ffffff28;
  transition:
    color 0.18s ease,
    background 0.18s ease,
    box-shadow 0.18s ease,
    transform 0.14s ease;
}

.login-submit:not(:disabled):hover {
  box-shadow:
    0 12px 30px -10px color-mix(in srgb, var(--cp-info) 70%, transparent),
    inset 0 1px 0 #ffffff36;
  transform: translateY(-1px);
}

.login-submit:not(:disabled):active {
  transform: translateY(0px);
  box-shadow:
    0 4px 14px -8px color-mix(in srgb, var(--cp-info) 50%, transparent),
    inset 0 1px 0 #ffffff18;
}

.login-submit:disabled {
  color: var(--cp-disabled-text);
  background: var(--cp-disabled-bg);
  box-shadow: none;
  transform: none;
}

/* ── Form footer ───────────────────────────────────────────────────────────── */
.login-form-footer {
  display: inline-flex;
  align-items: center;
  gap: 0;
  justify-content: center;
  padding-top: 2px;
}

.login-form-footer span {
  position: relative;
  display: inline-block;
  padding: 0 10px;
  color: var(--cp-text-muted);
  font-size: 11px;
  font-weight: 650;
  line-height: 1;
}

.login-form-footer span:first-child {
  padding-left: 0;
}

.login-form-footer span + span::before {
  position: absolute;
  top: 50%;
  left: 0;
  width: 1px;
  height: 9px;
  content: '';
  background: var(--cp-default-border);
  transform: translateY(-50%);
}

/* ── Responsive: tablet+ ───────────────────────────────────────────────────── */
@media (min-width: 720px) {
  .login-stage {
    width: min(calc(100% - 80px), 640px);
  }
}

/* ── Responsive: desktop ───────────────────────────────────────────────────── */
@media (min-width: 1100px) {
  .login-stage {
    width: min(calc(100% - 112px), 1320px);
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

  .login-copy-headline {
    max-width: 8em;
    font-size: clamp(46px, 3.6vw, 58px);
  }
}

/* ── Responsive: small mobile ──────────────────────────────────────────────── */
@media (max-width: 520px) {
  .login-page {
    background-size:
      48px 48px,
      48px 48px,
      auto;
  }

  .login-stage {
    gap: clamp(18px, 3.4vh, 26px);
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

  .login-form {
    padding: 24px 22px;
  }
}

/* ── Reduced motion ────────────────────────────────────────────────────────── */
@media (prefers-reduced-motion: reduce) {
  .login-page-wash {
    transform: none;
  }

  .login-orb {
    display: none;
  }
}
</style>
