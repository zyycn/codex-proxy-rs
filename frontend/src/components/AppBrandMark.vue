<script setup lang="ts">
import { useId } from 'vue'

const id = useId().replaceAll(':', '')
const noiseFilterId = `${id}-brand-noise`
const wornFilterId = `${id}-brand-worn`
const mistGradientId = `${id}-brand-mist`
</script>

<template>
  <svg class="text-cp-text-light-solid" xmlns="http://www.w3.org/2000/svg" viewBox="48 48 416 416" focusable="false">
    <defs>
      <filter :id="noiseFilterId" x="0" y="0" width="100%" height="100%">
        <feTurbulence type="fractalNoise" baseFrequency="1.7" numOctaves="2" seed="31" result="noise" />
        <feColorMatrix
          in="noise"
          type="matrix"
          values="0 0 0 0 1  0 0 0 0 1  0 0 0 0 1  2.56 2.56 2.56 0 -4.76"
          result="speckle"
        />
        <feComposite operator="in" in="SourceGraphic" in2="speckle" />
      </filter>
      <filter :id="wornFilterId" x="-12%" y="-12%" width="124%" height="124%">
        <feTurbulence type="fractalNoise" baseFrequency="0.12" numOctaves="2" seed="41" result="noise" />
        <feDisplacementMap in="SourceGraphic" in2="noise" scale="2.2" />
      </filter>
      <radialGradient :id="mistGradientId" cx="0.5" cy="0.45" r="0.75">
        <stop offset="0" stop-color="currentColor" stop-opacity="0.1" />
        <stop offset="0.55" stop-color="currentColor" stop-opacity="0.045" />
        <stop offset="1" stop-color="currentColor" stop-opacity="0.012" />
      </radialGradient>
    </defs>

    <rect class="fill-[var(--cp-brand-mark-bg)]" x="48" y="48" width="416" height="416" rx="88" />
    <g fill="none" stroke="currentColor" stroke-linecap="round" stroke-width="36">
      <path d="M156 307A112 112 0 0 1 307 156" />
      <path d="M356 205A112 112 0 0 1 205 356" />
    </g>
    <path d="m407 105-119 153-183 149 124-157Z" fill="currentColor" />

    <g :filter="`url(#${wornFilterId})`" fill="none" stroke="currentColor" stroke-linecap="square">
      <path d="M92 170 136 148" stroke-width="1" opacity="0.38" />
      <path d="M98 178 142 156" stroke-width="0.5" opacity="0.3" />
      <path d="M308 398 346 378" stroke-width="0.9" opacity="0.36" />
      <path d="M314 406 352 386" stroke-width="0.5" opacity="0.3" />
      <path d="M364 132 398 118" stroke-width="0.9" opacity="0.38" />
      <path d="M402 118 410 126" stroke-width="0.5" opacity="0.28" />
      <path d="M108 350 142 334" stroke-width="1" opacity="0.4" />
      <path d="M158 238 190 226" stroke-width="0.8" opacity="0.34" />
      <path d="M296 292 320 280" stroke-width="0.7" opacity="0.3" />
      <path d="M126 118 136 110" stroke-width="0.6" opacity="0.26" />
      <path d="M400 96 410 88" stroke-width="0.6" opacity="0.24" />
    </g>
    <g :filter="`url(#${wornFilterId})`" fill="currentColor">
      <path
        d="M98 176 104 173 108 175 114 170 118 173 124 167 130 170 136 164 142 167 148 161 154 164 160 158Z"
        opacity="0.34"
      />
      <path d="M314 395 320 392 324 394 330 389 336 392 342 386 348 389 354 383Z" opacity="0.3" />
    </g>

    <rect
      x="48"
      y="48"
      width="416"
      height="416"
      rx="88"
      fill="currentColor"
      :filter="`url(#${noiseFilterId})`"
      opacity="0.08"
    />
    <rect x="48" y="48" width="416" height="416" rx="88" :fill="`url(#${mistGradientId})`" />
  </svg>
</template>
