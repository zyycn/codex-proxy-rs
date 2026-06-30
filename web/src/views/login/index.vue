<script setup lang="ts">
import { storeToRefs } from 'pinia'
import { computed, shallowRef } from 'vue'
import { useRouter } from 'vue-router'
import { ArrowRight, Cat, CircleAlert, Eye, EyeOff, KeyRound, Mail, Moon, Sun } from '@lucide/vue'

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

const ingressProtocols = ['HTTP JSON', 'HTTP SSE', 'WS JSON', 'WS SSE']
const upstreamProtocols = ['RESPONSES', 'CHAT', 'REALTIME', 'TRACE']
const routeClusters = [
  { id: 'edge', labels: ['edge', 'cache', 'keys'], className: 'login-route-cluster--edge' },
  { id: 'retry', labels: ['retry', 'trace', 'sse'], className: 'login-route-cluster--retry' },
  { id: 'audit', labels: ['audit', 'events', 'logs'], className: 'login-route-cluster--audit' },
  { id: 'model', labels: ['model', 'tools', 'quota'], className: 'login-route-cluster--model' },
]

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
</script>

<template>
  <main class="login-page">
    <div class="login-bg" aria-hidden="true">
      <div class="login-bg-base" />
      <div class="login-bg-grid" />
      <div class="login-bg-striations" />
      <div class="login-bg-grain" />

      <span class="login-watermark">CODEX_PROXY_RS // ROUTE_TOPOLOGY // AUTH_GATEWAY</span>

      <div class="login-protocol-stack login-protocol-stack--ingress">
        <span v-for="protocol in ingressProtocols" :key="protocol">{{ protocol }}</span>
      </div>

      <div class="login-protocol-stack login-protocol-stack--upstream">
        <span v-for="protocol in upstreamProtocols" :key="protocol">{{ protocol }}</span>
      </div>

      <svg
        class="login-topology"
        viewBox="0 0 1920 1080"
        preserveAspectRatio="xMidYMid slice"
        focusable="false"
        aria-hidden="true"
      >
        <g class="login-route-lines">
          <path
            class="login-route-line--bundle"
            d="M0 32c132 0 166 48 270 84 94 32 202 32 370 30 172 0 276-8 410-28 148-22 229-74 329-76m-1379 50c112 0 170 12 274 36 96 22 222 22 366 18 178 0 286 4 426 0 148-4 262-30 313-32m-1379 42c132 0 184-12 294-14 126-2 216 4 346 4 176 0 282 22 426 38 166 20 280 36 313 36m-1379 4c160 0 202-34 316-60 108-24 204-18 324-18 140 0 262 12 378 26 102 12 216 22 361 16"
            transform="translate(286 382)"
          />
          <path
            class="login-route-line--stream"
            d="M1030 48c-170-30-320-20-478 30-160 52-292 48-552 10m870-26c-150 10-242 42-358 64-126 24-260 28-440 16"
            transform="translate(510 316)"
          />
          <path
            class="login-route-line--audit"
            d="M980 24c-196 54-352 66-512 34-138-28-294-16-468 30m690-24c-134 26-262 30-380 12-106-16-194-8-276 20"
            transform="translate(440 704)"
          />
        </g>

        <g class="login-particle-tracks">
          <path
            id="login-track-a"
            d="M286 414 C418 414 452 462 556 498 C650 530 758 530 926 528 C1098 528 1202 520 1336 500 C1484 478 1565 426 1665 424"
          />
          <path
            id="login-track-b"
            d="M286 474 C398 474 456 486 560 510 C656 532 782 532 926 528 C1104 528 1212 532 1352 528 C1500 524 1614 498 1665 496"
          />
          <path
            id="login-track-c"
            d="M286 538 C418 538 470 526 580 524 C706 522 796 528 926 528 C1102 528 1208 550 1352 566 C1518 586 1632 602 1665 602"
          />
          <path
            id="login-track-d"
            d="M286 606 C446 606 488 572 602 546 C710 522 806 528 926 528 C1066 528 1188 540 1304 554 C1406 566 1520 576 1665 570"
          />
          <path
            id="login-track-return"
            d="M1540 364 C1370 334 1220 344 1062 394 C902 446 770 442 510 404"
          />
        </g>

        <g class="login-particle">
          <circle r="3.2" />
          <animateMotion dur="7.2s" repeatCount="indefinite" rotate="auto">
            <mpath href="#login-track-a" />
          </animateMotion>
          <animate
            attributeName="opacity"
            values="0;1;1;0"
            keyTimes="0;0.12;0.82;1"
            dur="7.2s"
            repeatCount="indefinite"
          />
        </g>
        <g class="login-particle">
          <circle r="2.8" />
          <animateMotion dur="8.8s" begin="-2.1s" repeatCount="indefinite" rotate="auto">
            <mpath href="#login-track-b" />
          </animateMotion>
          <animate
            attributeName="opacity"
            values="0;0.82;0.82;0"
            keyTimes="0;0.14;0.82;1"
            dur="8.8s"
            begin="-2.1s"
            repeatCount="indefinite"
          />
        </g>
        <g class="login-particle">
          <circle r="2.6" />
          <animateMotion dur="9.4s" begin="-4.4s" repeatCount="indefinite" rotate="auto">
            <mpath href="#login-track-c" />
          </animateMotion>
          <animate
            attributeName="opacity"
            values="0;0.78;0.78;0"
            keyTimes="0;0.13;0.84;1"
            dur="9.4s"
            begin="-4.4s"
            repeatCount="indefinite"
          />
        </g>
        <g class="login-particle">
          <circle r="2.5" />
          <animateMotion dur="10.8s" begin="-5.2s" repeatCount="indefinite" rotate="auto">
            <mpath href="#login-track-d" />
          </animateMotion>
          <animate
            attributeName="opacity"
            values="0;0.7;0.7;0"
            keyTimes="0;0.16;0.8;1"
            dur="10.8s"
            begin="-5.2s"
            repeatCount="indefinite"
          />
        </g>
        <g class="login-particle">
          <circle r="2.3" />
          <animateMotion dur="11.4s" begin="-1.8s" repeatCount="indefinite" rotate="auto">
            <mpath href="#login-track-return" />
          </animateMotion>
          <animate
            attributeName="opacity"
            values="0;0.55;0.55;0"
            keyTimes="0;0.16;0.78;1"
            dur="11.4s"
            begin="-1.8s"
            repeatCount="indefinite"
          />
        </g>

        <g class="login-packets login-packets--semantic">
          <circle cx="420" cy="410" r="3" />
          <circle cx="626" cy="524" r="3" />
          <circle cx="812" cy="524" r="3" />
          <circle cx="1262" cy="502" r="3" />
          <circle cx="1540" cy="422" r="3" />
          <circle cx="614" cy="459" r="2.5" />
          <circle cx="780" cy="528" r="2.5" />
          <circle cx="982" cy="528" r="2.5" />
          <circle cx="1390" cy="496" r="2.5" />
        </g>
      </svg>

      <div
        v-for="cluster in routeClusters"
        :key="cluster.id"
        class="login-route-cluster"
        :class="cluster.className"
      >
        <span v-for="label in cluster.labels" :key="label">
          <i aria-hidden="true" />
          {{ label }}
        </span>
      </div>
    </div>

    <section class="login-stage" aria-label="Codex Proxy RS 登录">
      <BaseCard
        as="form"
        :padded="false"
        variant="elevated"
        class="login-form"
        @submit.prevent="handleSubmit"
      >
        <div class="login-form-line" aria-hidden="true" />

        <header class="login-topbar">
          <div class="login-brand">
            <span class="login-logo">
              <Cat :size="20" :stroke-width="2.1" />
            </span>
            <span class="login-brand-text">
              <strong>Codex Proxy RS</strong>
              <span>ADMIN REALM</span>
            </span>
          </div>

          <button
            class="login-theme-toggle"
            :class="{ 'is-dark': effectiveTheme === 'dark' }"
            type="button"
            :aria-label="themeToggleLabel"
            :title="themeToggleLabel"
            @click="toggleTheme($event)"
          >
            <Sun :size="16" />
            <span class="login-theme-knob" aria-hidden="true" />
            <Moon :size="16" />
          </button>
        </header>

        <section class="login-form-header" aria-labelledby="login-title">
          <h1 id="login-title">控制台登录</h1>
          <p>使用管理员账号进入 Codex Proxy RS，管理路由、密钥、账号池与链路观测。</p>
        </section>

        <div class="login-fields">
          <div v-if="authStore.error" class="login-error" role="alert">
            <CircleAlert :size="16" />
            <p>{{ authStore.error }}</p>
          </div>

          <label class="login-field-group">
            <span>管理员账号</span>
            <BaseInput v-model="username" placeholder="admin@example.com" autocomplete="username">
              <template #prefix>
                <Mail :size="17" />
              </template>
            </BaseInput>
          </label>

          <label class="login-field-group">
            <span>访问密钥</span>
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
              <span>{{ authStore.loading ? '正在进入...' : '进入控制台' }}</span>
              <ArrowRight v-if="!authStore.loading" :size="18" />
            </BaseButton>
          </div>

          <footer class="login-form-footer">
            <span>Zzz</span>
            <span>v0.1 admin realm</span>
          </footer>
        </div>
      </BaseCard>
    </section>
  </main>
</template>

<style scoped>
.login-page {
  --login-page-a: #f7fafc;
  --login-page-b: #eef4fa;
  --login-page-c: #e6eef5;
  --login-base-a: #ffffff;
  --login-base-b: #f3f7fb;
  --login-base-c: #e8eff6;
  --login-edge-mid: #c8d7e733;
  --login-edge-end: #8da1b840;
  --login-grid: #5b708607;
  --login-striation: #5b708632;
  --login-grain: #60798f22;
  --login-grain-alt-a: #60798f22;
  --login-grain-alt-b: #60798f22;
  --login-route-bundle: #2e526b32;
  --login-route-stream: #2f7fb028;
  --login-route-audit: #52697d24;
  --login-semantic: #2f7fb044;
  --login-particle: #2f7fb0;
  --login-particle-glow: #2f7fb099;
  --login-watermark: #2f465d8c;
  --login-stack-text: #294157d8;
  --login-stack-bg: #ffffffb8;
  --login-stack-dot: #1f78aa9e;
  --login-stack-opacity: 0.55;
  --login-stack-upstream-opacity: 0.62;
  --login-cluster-bg: #ffffffb8;
  --login-cluster-pulse: #1f78aaa8;
  --login-cluster-text: #294157c8;
  --login-form-bg-a: #fffffff2;
  --login-form-bg-b: #f8fbffe8;
  --login-form-bg-c: #edf3f9ea;
  --login-form-shadow: #27415633;
  --login-form-line: #6e8fae66;
  --login-logo-bg: #e7f0f8e6;
  --login-logo-text: #2d6f98;
  --login-title: #101a27;
  --login-brand-title: #172536;
  --login-brand-caption: #64748b;
  --login-description: #536579;
  --login-label: #243447;
  --login-input-bg: #f3f7fb;
  --login-input-bg-hover: #edf4fa;
  --login-input-focus: #ffffff;
  --login-input-icon: #2f6f98;
  --login-placeholder: #64748b;
  --login-error-bg: #fff1f2;
  --login-error-icon: #dc2626;
  --login-error-text: #b91c1c;
  --login-button: #2f7fb0;
  --login-button-hover: #3b90c5;
  --login-button-active: #256c99;
  --login-button-shadow: #2f7fb04d;
  --login-footer: #66798c;
  --login-toggle-bg: #e7eef6e6;
  --login-toggle-sun: #2d6f98;
  --login-toggle-moon: #64748b;
  --login-toggle-knob: #ffffff;
  --login-toggle-shadow: #2d6f9833;
  --login-disabled-bg: #e5edf4;
  --login-disabled-text: #8797a8;

  position: relative;
  min-height: 100dvh;
  overflow: hidden;
  color: var(--login-title);
  background: linear-gradient(
    118deg,
    var(--login-page-a),
    var(--login-page-b) 52%,
    var(--login-page-c)
  );
  isolation: isolate;
}

:global(html[data-theme='dark'] .login-page) {
  --login-page-a: #0a1420;
  --login-page-b: #101c2b;
  --login-page-c: #081a1d;
  --login-base-a: #172638;
  --login-base-b: #0d1722;
  --login-base-c: #05080c;
  --login-edge-mid: #00000034;
  --login-edge-end: #000000a8;
  --login-grid: #6f91ab0c;
  --login-striation: #6f91ab0c;
  --login-grain: #9fb8c910;
  --login-grain-alt-a: #6f91ab12;
  --login-grain-alt-b: #d6eaf50a;
  --login-route-bundle: #b3d9f02a;
  --login-route-stream: #8fcbea24;
  --login-route-audit: #b3d9f01f;
  --login-semantic: #9bcfea44;
  --login-particle: #9ee7ff;
  --login-particle-glow: #9ee7ffcc;
  --login-watermark: #b3d9f05f;
  --login-stack-text: #dcebface;
  --login-stack-bg: #1422338a;
  --login-stack-dot: #7cc7eaba;
  --login-stack-opacity: 0.76;
  --login-stack-upstream-opacity: 0.76;
  --login-cluster-bg: #14223386;
  --login-cluster-pulse: #6ec4e9b0;
  --login-cluster-text: #c6e4f2a8;
  --login-form-bg-a: #243244e0;
  --login-form-bg-b: #1a2837ea;
  --login-form-bg-c: #121d2be8;
  --login-form-shadow: #00000080;
  --login-form-line: #b3d9f0aa;
  --login-logo-bg: #101b26d9;
  --login-logo-text: #b3d9f0;
  --login-title: #f7fbff;
  --login-brand-title: #ffffff;
  --login-brand-caption: #888888;
  --login-description: #a8b6c3;
  --login-label: #d8e6f5;
  --login-input-bg: #0c1621dd;
  --login-input-bg-hover: #111d2bdd;
  --login-input-focus: #132334e8;
  --login-input-icon: #7dd3fc;
  --login-placeholder: #6b7d90;
  --login-error-bg: #ef44441a;
  --login-error-icon: #fca5a5;
  --login-error-text: #fecaca;
  --login-button: #3f8ec6;
  --login-button-hover: #4a9fd8;
  --login-button-active: #317bab;
  --login-button-shadow: #3f8ec64d;
  --login-footer: #7f8ea3;
  --login-toggle-bg: #101b26d9;
  --login-toggle-sun: #888888;
  --login-toggle-moon: #ffffff;
  --login-toggle-knob: #4a9fd8;
  --login-toggle-shadow: #4a9fd84d;
  --login-disabled-bg: #1b2735;
  --login-disabled-text: #6b7d90;
}

.login-bg {
  position: absolute;
  inset: 0;
  z-index: -1;
  overflow: hidden;
  pointer-events: none;
}

.login-bg-base,
.login-bg-base::after,
.login-bg-grid,
.login-bg-striations,
.login-bg-grain,
.login-topology {
  position: absolute;
  inset: 0;
}

.login-bg-base {
  background:
    radial-gradient(
      ellipse 59% 48% at 55% 48%,
      var(--login-base-a) 0%,
      var(--login-base-b) 50%,
      var(--login-base-c) 100%
    ),
    var(--login-base-c);
}

.login-bg-base::after {
  content: '';
  background: radial-gradient(
    ellipse 52.5% 44% at 54% 47%,
    transparent 0%,
    var(--login-edge-mid) 72%,
    var(--login-edge-end) 100%
  );
}

.login-bg-grid {
  background:
    repeating-linear-gradient(90deg, var(--login-grid) 0 1px, transparent 1px 80px),
    repeating-linear-gradient(180deg, var(--login-grid) 0 1px, transparent 1px 80px);
}

.login-bg-striations {
  background:
    linear-gradient(var(--login-striation), var(--login-striation)) 0 126px / 100% 1px no-repeat,
    linear-gradient(var(--login-striation), var(--login-striation)) 0 214px / 100% 1px no-repeat,
    linear-gradient(var(--login-striation), var(--login-striation)) 0 338px / 100% 1px no-repeat,
    linear-gradient(var(--login-striation), var(--login-striation)) 0 462px / 100% 1px no-repeat,
    linear-gradient(var(--login-striation), var(--login-striation)) 0 586px / 100% 1px no-repeat,
    linear-gradient(var(--login-striation), var(--login-striation)) 0 714px / 100% 1px no-repeat,
    linear-gradient(var(--login-striation), var(--login-striation)) 0 846px / 100% 1px no-repeat,
    linear-gradient(var(--login-striation), var(--login-striation)) 0 982px / 100% 1px no-repeat;
}

.login-bg-grain {
  opacity: 0.68;
  background-image:
    linear-gradient(var(--login-grain), var(--login-grain)) 139px 109px / 3px 2px no-repeat,
    linear-gradient(var(--login-grain-alt-a), var(--login-grain-alt-a)) 288px 192px / 1px 1px
      no-repeat,
    linear-gradient(var(--login-grain-alt-b), var(--login-grain-alt-b)) 437px 275px / 1px 1px
      no-repeat,
    linear-gradient(var(--login-grain), var(--login-grain)) 586px 358px / 2px 1px no-repeat,
    linear-gradient(var(--login-grain-alt-a), var(--login-grain-alt-a)) 735px 441px / 1px 2px
      no-repeat,
    linear-gradient(var(--login-grain-alt-b), var(--login-grain-alt-b)) 884px 524px / 3px 1px
      no-repeat,
    linear-gradient(var(--login-grain), var(--login-grain)) 1033px 607px / 2px 1px no-repeat,
    linear-gradient(var(--login-grain-alt-a), var(--login-grain-alt-a)) 1182px 690px / 1px 1px
      no-repeat,
    linear-gradient(var(--login-grain-alt-b), var(--login-grain-alt-b)) 1331px 773px / 1px 2px
      no-repeat,
    linear-gradient(var(--login-grain), var(--login-grain)) 1480px 856px / 2px 1px no-repeat,
    linear-gradient(var(--login-grain-alt-a), var(--login-grain-alt-a)) 1629px 939px / 3px 1px
      no-repeat,
    linear-gradient(var(--login-grain-alt-b), var(--login-grain-alt-b)) 1778px 67px / 1px 1px
      no-repeat;
}

.login-watermark {
  position: absolute;
  top: 9.63%;
  left: 6.15%;
  color: var(--login-watermark);
  font-family: var(--font-mono);
  font-size: 13px;
  font-weight: 500;
  line-height: 1;
  white-space: nowrap;
}

.login-protocol-stack {
  position: absolute;
  display: grid;
  gap: 8px;
  width: 156px;
  color: var(--login-stack-text);
  font-family: var(--font-mono);
  font-size: 11px;
  font-weight: 500;
  line-height: 1;
}

.login-protocol-stack--ingress {
  top: 36.1111%;
  left: 6.5625%;
  --login-stack-item-opacity: var(--login-stack-opacity);
}

.login-protocol-stack--upstream {
  top: 36.1111%;
  right: 3.4375%;
  --login-stack-item-opacity: var(--login-stack-upstream-opacity);
}

.login-protocol-stack span {
  display: inline-flex;
  width: fit-content;
  align-items: center;
  gap: 8px;
  border-radius: 4px;
  background: var(--login-stack-bg);
  padding: 7px 10px;
  opacity: var(--login-stack-item-opacity);
}

.login-protocol-stack span::before {
  content: '';
  width: 6px;
  height: 6px;
  flex: 0 0 auto;
  border-radius: 50%;
  background: var(--login-stack-dot);
}

.login-topology {
  width: 100%;
  height: 100%;
}

.login-topology path,
.login-topology ellipse,
.login-topology circle {
  vector-effect: non-scaling-stroke;
}

.login-route-lines path {
  fill: transparent;
  stroke-width: 1;
  stroke-linecap: round;
}

.login-route-line--bundle {
  stroke: var(--login-route-bundle);
}

.login-route-line--stream {
  stroke: var(--login-route-stream);
}

.login-route-line--audit {
  stroke: var(--login-route-audit);
}

.login-particle-tracks path {
  fill: transparent;
  stroke: none;
}

.login-particle {
  opacity: 0;
  filter: drop-shadow(0 0 8px var(--login-particle-glow));
}

.login-particle circle {
  fill: var(--login-particle);
}

.login-packets--semantic circle {
  fill: var(--login-semantic);
}

.login-route-cluster {
  position: absolute;
  display: grid;
  gap: 6px;
  color: var(--login-cluster-text);
  font-family: var(--font-mono);
  font-size: 10px;
  font-weight: 500;
  line-height: 1;
}

.login-route-cluster span {
  display: inline-flex;
  width: fit-content;
  align-items: center;
  gap: 8px;
  border-radius: 4px;
  background: var(--login-cluster-bg);
  padding: 4px 8px;
}

.login-route-cluster i {
  display: inline-block;
  width: 18px;
  height: 2px;
  border-radius: 2px;
  background: var(--login-cluster-pulse);
}

.login-route-cluster--edge {
  top: 28.2407%;
  left: 23.4375%;
}

.login-route-cluster--retry {
  top: 26.2963%;
  left: 63.0208%;
}

.login-route-cluster--audit {
  top: 69.4444%;
  left: 23.6979%;
}

.login-route-cluster--model {
  top: 68.7037%;
  left: 66.6667%;
}

.login-stage {
  box-sizing: border-box;
  display: grid;
  min-height: 100dvh;
  padding: clamp(24px, 5vh, 64px) clamp(20px, 4vw, 64px);
  align-items: center;
  justify-items: center;
}

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
  --cp-info-on: #ffffff;
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

  position: relative;
  display: grid;
  width: min(420px, 100%);
  min-height: 517px;
  gap: 18px;
  padding: 26px 30px 24px;
  border-radius: 8px;
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
  background: linear-gradient(90deg, #ffffff00, var(--login-form-line), #ffffff00);
  opacity: 0.42;
  pointer-events: none;
}

:global(html[data-theme='dark'] .login-form-line) {
  opacity: 0.3;
}

.login-topbar {
  display: flex;
  min-width: 0;
  align-items: center;
  justify-content: space-between;
  gap: 18px;
}

.login-brand {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: 12px;
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

.login-brand-text {
  display: grid;
  min-width: 0;
  gap: 4px;
}

.login-brand-text strong {
  color: var(--login-brand-title);
  font-size: 17px;
  font-weight: 600;
  line-height: 1.12;
}

.login-brand-text span {
  color: var(--login-brand-caption);
  font-family: var(--font-mono);
  font-size: 10px;
  font-weight: 400;
  line-height: 1.2;
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

.login-form-header {
  display: grid;
  gap: 10px;
  min-width: 0;
}

.login-form-header h1 {
  margin: 0;
  color: var(--login-title);
  font-size: 34px;
  font-weight: 600;
  line-height: 1.02;
}

.login-form-header p {
  margin: 0;
  color: var(--login-description);
  font-size: 14px;
  font-weight: 400;
  line-height: 1.45;
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

.login-fields {
  display: grid;
  gap: 12px;
}

.login-field-group {
  display: grid;
  min-width: 0;
  gap: 8px;
}

.login-field-group > span {
  color: var(--login-label);
  font-size: 13px;
  font-weight: 700;
  line-height: 1.1;
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

.login-submit-slot {
  min-width: 0;
}

.login-submit {
  width: 100%;
  height: 48px;
  background: var(--login-button);
  box-shadow: 0 14px 24px -18px var(--login-button-shadow);
  transition:
    background 0.16s ease,
    box-shadow 0.16s ease,
    transform 0.14s ease;
}

.login-submit:not(:disabled):hover {
  background: var(--login-button-hover);
  box-shadow: 0 16px 30px -17px var(--login-button-shadow);
  transform: translateY(-1px);
}

.login-submit:not(:disabled):active {
  background: var(--login-button-active);
  box-shadow: 0 10px 18px -18px var(--login-button-shadow);
  transform: translateY(0);
}

.login-submit:disabled {
  background: var(--login-disabled-bg);
  box-shadow: none;
  transform: none;
}

.login-form-footer {
  display: flex;
  min-width: 0;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
}

.login-form-footer span {
  min-width: 0;
  color: var(--login-footer);
  font-family: var(--font-mono);
  font-size: 10px;
  font-weight: 700;
  line-height: 1.2;
}

.login-form-footer span:last-child {
  text-align: right;
}

@media (min-width: 980px) {
  .login-stage {
    justify-items: end;
    padding-right: clamp(48px, 17.3vw, 332px);
  }
}

@media (max-width: 720px) {
  .login-watermark,
  .login-protocol-stack,
  .login-route-cluster {
    opacity: 0.48;
  }
}

@media (max-width: 560px) {
  .login-stage {
    padding: 18px;
  }

  .login-form {
    min-height: auto;
    gap: 16px;
    padding: 22px;
  }

  .login-topbar {
    gap: 14px;
  }

  .login-brand-text strong {
    font-size: 15px;
  }

  .login-form-header h1 {
    font-size: 30px;
  }

  .login-form-footer {
    align-items: flex-start;
    flex-direction: column;
    gap: 6px;
  }

  .login-form-footer span:last-child {
    text-align: left;
  }
}

@media (prefers-reduced-motion: reduce) {
  .login-particle {
    display: none;
  }

  .login-theme-knob,
  .login-submit,
  .login-theme-toggle {
    transition: none;
  }
}
</style>
