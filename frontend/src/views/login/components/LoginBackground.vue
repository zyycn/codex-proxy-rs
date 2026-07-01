<script setup lang="ts">
type RouteCluster = {
  id: string
  labels: readonly string[]
  className: string
}

const ingressProtocols = ['HTTP JSON', 'HTTP SSE', 'WS JSON', 'WS SSE'] as const
const upstreamProtocols = ['RESPONSES', 'CHAT', 'REALTIME', 'TRACE'] as const
const routeClusters: readonly RouteCluster[] = [
  { id: 'edge', labels: ['edge', 'cache', 'keys'], className: 'login-route-cluster--edge' },
  { id: 'retry', labels: ['retry', 'trace', 'sse'], className: 'login-route-cluster--retry' },
  { id: 'audit', labels: ['audit', 'events', 'logs'], className: 'login-route-cluster--audit' },
  { id: 'model', labels: ['model', 'tools', 'quota'], className: 'login-route-cluster--model' },
]
</script>

<template>
  <div class="pointer-events-none absolute inset-0 z-[-1] overflow-hidden" aria-hidden="true">
    <div class="login-bg-base" />
    <div class="login-bg-grid" />
    <div class="login-bg-striations" />
    <div class="login-bg-grain" />

    <span
      class="login-watermark absolute top-[9.63%] left-[6.15%] whitespace-nowrap font-mono text-[13px] leading-none font-medium text-(--login-watermark)"
    >
      CODEX_PROXY_RS // ROUTE_TOPOLOGY // AUTH_GATEWAY
    </span>

    <div class="login-protocol-stack login-protocol-stack--ingress">
      <span
        v-for="protocol in ingressProtocols"
        :key="protocol"
        class="inline-flex w-fit items-center gap-2 rounded-sm bg-(--login-stack-bg) px-2.5 py-1.75 opacity-(--login-stack-item-opacity) before:size-1.5 before:shrink-0 before:rounded-full before:bg-(--login-stack-dot) before:content-['']"
      >
        {{ protocol }}
      </span>
    </div>

    <div class="login-protocol-stack login-protocol-stack--upstream">
      <span
        v-for="protocol in upstreamProtocols"
        :key="protocol"
        class="inline-flex w-fit items-center gap-2 rounded-sm bg-(--login-stack-bg) px-2.5 py-1.75 opacity-(--login-stack-item-opacity) before:size-1.5 before:shrink-0 before:rounded-full before:bg-(--login-stack-dot) before:content-['']"
      >
        {{ protocol }}
      </span>
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
      class="login-route-cluster absolute grid gap-1.5 font-mono text-[10px] leading-none font-medium text-(--login-cluster-text)"
      :class="cluster.className"
    >
      <span
        v-for="label in cluster.labels"
        :key="label"
        class="inline-flex w-fit items-center gap-2 rounded-sm bg-(--login-cluster-bg) px-2 py-1"
      >
        <i
          class="inline-block h-0.5 w-4.5 rounded-xs bg-(--login-cluster-pulse)"
          aria-hidden="true"
        />
        {{ label }}
      </span>
    </div>
  </div>
</template>

<style scoped>
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

@media (max-width: 720px) {
  .login-watermark,
  .login-protocol-stack,
  .login-route-cluster {
    opacity: 0.48;
  }
}

@media (prefers-reduced-motion: reduce) {
  .login-particle {
    display: none;
  }
}
</style>
