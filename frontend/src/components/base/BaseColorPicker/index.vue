<script setup lang="ts">
import { Check, ChevronDown } from '@lucide/vue'
import { clamp } from 'es-toolkit'
import { computed, shallowRef, useTemplateRef, watch } from 'vue'

import { normalizeHexColor } from '@/utils/color'
import BaseButton from '../BaseButton.vue'
import BaseInput from '../BaseInput.vue'
import BasePopover from '../BasePopover.vue'
import { formatRgbaColor, hexToHsva, hsvaToHex, normalizePickerHexColor, parseRgbaColor, rgbaToHexColor } from './color'

const props = withDefaults(
  defineProps<{
    presets?: readonly string[]
    disabled?: boolean
    label?: string
    allowAlpha?: boolean
  }>(),
  {
    presets: () => [],
    disabled: false,
    label: '选择颜色',
    allowAlpha: true,
  },
)

const model = defineModel<string>({ required: true })
const open = shallowRef(false)
const hue = shallowRef(0)
const saturation = shallowRef(100)
const brightness = shallowRef(100)
const alpha = shallowRef(0.5)
const customInput = shallowRef('')
const svPanel = useTemplateRef<HTMLElement>('svPanel')
const hueSlider = useTemplateRef<HTMLElement>('hueSlider')
const alphaSlider = useTemplateRef<HTMLElement>('alphaSlider')

const normalizedModel = computed(
  () => normalizePickerHexColor(model.value, props.allowAlpha) ?? (props.allowAlpha ? '#60A5FA80' : '#2563EB'),
)
const triggerSwatchStyle = computed(() =>
  props.allowAlpha
    ? {
        backgroundImage: `linear-gradient(${normalizedModel.value}, ${normalizedModel.value}), conic-gradient(#d8dee8 25%, #fff 0 50%, #d8dee8 0 75%, #fff 0)`,
        backgroundSize: '100% 100%, 8px 8px',
      }
    : { backgroundColor: normalizedModel.value },
)
const draftColor = computed(() =>
  hsvaToHex(
    {
      h: hue.value,
      s: saturation.value,
      v: brightness.value,
      a: alpha.value,
    },
    props.allowAlpha,
  ),
)
const hueColor = computed(() => `hsl(${hue.value} 100% 50%)`)
const svCursorStyle = computed(() => ({
  left: `${saturation.value}%`,
  top: `${100 - brightness.value}%`,
}))
const hueThumbStyle = computed(() => ({ top: `${(hue.value / 360) * 100}%` }))
const alphaThumbStyle = computed(() => ({ left: `${alpha.value * 100}%` }))
const alphaTrackStyle = computed(() => {
  const hex = draftColor.value
  const red = Number.parseInt(hex.slice(1, 3), 16)
  const green = Number.parseInt(hex.slice(3, 5), 16)
  const blue = Number.parseInt(hex.slice(5, 7), 16)
  return {
    backgroundImage: `linear-gradient(to right, rgb(${red} ${green} ${blue} / 0), rgb(${red} ${green} ${blue} / 1)), conic-gradient(#d8dee8 25%, #fff 0 50%, #d8dee8 0 75%, #fff 0)`,
    backgroundSize: '100% 100%, 8px 8px',
  }
})
const inputValid = computed(() =>
  props.allowAlpha ? Boolean(parseRgbaColor(customInput.value)) : Boolean(normalizeHexColor(customInput.value)),
)
const inputError = computed(() =>
  customInput.value && !inputValid.value
    ? props.allowAlpha
      ? '请输入 rgba(r, g, b, a)'
      : '请输入六位 HEX 颜色，例如 #2563EB'
    : undefined,
)

watch(open, (isOpen) => {
  if (isOpen)
    resetDraft(normalizedModel.value)
})

watch(model, (value) => {
  if (!open.value)
    resetDraft(value)
})

function resetDraft(value: string) {
  const color = hexToHsva(normalizePickerHexColor(value, props.allowAlpha) ?? '') ?? {
    h: 213,
    s: 62,
    v: 98,
    a: props.allowAlpha ? 0.5 : 1,
  }
  hue.value = color.h
  saturation.value = color.s
  brightness.value = color.v
  alpha.value = props.allowAlpha ? color.a : 1
  syncInput()
}

function syncInput() {
  customInput.value = props.allowAlpha
    ? (formatRgbaColor(draftColor.value) ?? '')
    : (normalizeHexColor(draftColor.value) ?? '')
}

function updateSaturationBrightness(event: PointerEvent) {
  const panel = svPanel.value
  if (!panel)
    return

  const bounds = panel.getBoundingClientRect()
  saturation.value = clamp(((event.clientX - bounds.left) / bounds.width) * 100, 0, 100)
  brightness.value = clamp(100 - ((event.clientY - bounds.top) / bounds.height) * 100, 0, 100)
  syncInput()
}

function startSaturationBrightness(event: PointerEvent) {
  if (props.disabled)
    return
  event.preventDefault()
  svPanel.value?.setPointerCapture(event.pointerId)
  updateSaturationBrightness(event)
}

function updateHue(event: PointerEvent) {
  const slider = hueSlider.value
  if (!slider)
    return

  const bounds = slider.getBoundingClientRect()
  hue.value = clamp(((event.clientY - bounds.top) / bounds.height) * 360, 0, 360)
  syncInput()
}

function startHue(event: PointerEvent) {
  if (props.disabled)
    return
  event.preventDefault()
  hueSlider.value?.setPointerCapture(event.pointerId)
  updateHue(event)
}

function handleSaturationBrightnessKeydown(event: KeyboardEvent) {
  const step = event.shiftKey ? 5 : 1
  if (event.key === 'ArrowLeft')
    saturation.value = clamp(saturation.value - step, 0, 100)
  else if (event.key === 'ArrowRight')
    saturation.value = clamp(saturation.value + step, 0, 100)
  else if (event.key === 'ArrowUp')
    brightness.value = clamp(brightness.value + step, 0, 100)
  else if (event.key === 'ArrowDown')
    brightness.value = clamp(brightness.value - step, 0, 100)
  else return

  event.preventDefault()
  syncInput()
}

function handleHueKeydown(event: KeyboardEvent) {
  const step = event.shiftKey ? 10 : 1
  if (event.key === 'ArrowUp' || event.key === 'ArrowLeft')
    hue.value = clamp(hue.value - step, 0, 360)
  else if (event.key === 'ArrowDown' || event.key === 'ArrowRight')
    hue.value = clamp(hue.value + step, 0, 360)
  else return

  event.preventDefault()
  syncInput()
}

function updateAlpha(event: PointerEvent) {
  const slider = alphaSlider.value
  if (!slider)
    return

  const bounds = slider.getBoundingClientRect()
  alpha.value = clamp((event.clientX - bounds.left) / bounds.width, 0, 1)
  syncInput()
}

function startAlpha(event: PointerEvent) {
  if (props.disabled)
    return
  event.preventDefault()
  alphaSlider.value?.setPointerCapture(event.pointerId)
  updateAlpha(event)
}

function handleAlphaKeydown(event: KeyboardEvent) {
  const step = event.shiftKey ? 0.1 : 0.01
  if (event.key === 'ArrowLeft' || event.key === 'ArrowDown')
    alpha.value = clamp(alpha.value - step, 0, 1)
  else if (event.key === 'ArrowRight' || event.key === 'ArrowUp')
    alpha.value = clamp(alpha.value + step, 0, 1)
  else return

  event.preventDefault()
  syncInput()
}

function updateFromInput() {
  const inputColor = normalizedInputColor()
  const color = inputColor ? hexToHsva(inputColor) : null
  if (!color)
    return

  hue.value = color.h
  saturation.value = color.s
  brightness.value = color.v
  alpha.value = props.allowAlpha ? color.a : 1
  syncInput()
}

function choosePreset(value: string) {
  const normalized = normalizePickerHexColor(value, props.allowAlpha)
  if (normalized)
    resetDraft(normalized)
}

function isPresetSelected(value: string) {
  const normalized = normalizePickerHexColor(value, props.allowAlpha)
  return normalized?.slice(0, 7) === draftColor.value.slice(0, 7)
}

function presetCheckClass(value: string) {
  const normalized = normalizePickerHexColor(value, props.allowAlpha)
  if (!normalized)
    return 'text-white'

  const red = Number.parseInt(normalized.slice(1, 3), 16)
  const green = Number.parseInt(normalized.slice(3, 5), 16)
  const blue = Number.parseInt(normalized.slice(5, 7), 16)
  const brightness = red * 0.299 + green * 0.587 + blue * 0.114

  return brightness > 160 ? 'text-[#0f172a]' : 'text-white'
}

function confirm() {
  updateFromInput()
  const normalized = normalizedInputColor()
  if (!normalized)
    return

  model.value = normalized
  open.value = false
}

function normalizedInputColor() {
  if (!props.allowAlpha)
    return normalizeHexColor(customInput.value)

  const rgba = parseRgbaColor(customInput.value)
  return rgba ? rgbaToHexColor(rgba) : null
}
</script>

<template>
  <BasePopover v-model="open" class="p-0.75" placement="bottom-start" :disabled="props.disabled">
    <template #trigger>
      <button
        type="button"
        class="group inline-flex size-8 items-center justify-center rounded-sm border-0 bg-transparent p-0 outline-none transition-opacity duration-150 hover:opacity-90 focus-visible:ring-2 focus-visible:ring-cp-control-outline disabled:cursor-not-allowed disabled:opacity-60"
        :disabled="props.disabled"
        :aria-label="props.label"
        aria-haspopup="dialog"
        :aria-expanded="open"
      >
        <span
          class="relative flex size-full items-center justify-center overflow-hidden rounded-sm"
          :style="triggerSwatchStyle"
        >
          <ChevronDown class="relative size-3.5 text-white drop-shadow-[0_1px_2px_rgb(0_0_0/0.65)]" />
        </span>
      </button>
    </template>

    <div class="grid w-76 gap-3 p-3" role="dialog" :aria-label="props.label">
      <div class="grid h-43 grid-cols-[1fr_14px] gap-2">
        <div
          ref="svPanel"
          class="base-color-picker__sv relative touch-none overflow-hidden rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
          :style="{ backgroundColor: hueColor }"
          role="slider"
          tabindex="0"
          aria-label="饱和度和明度"
          aria-valuemin="0"
          aria-valuemax="100"
          :aria-valuenow="Math.round(saturation)"
          :aria-valuetext="`饱和度 ${Math.round(saturation)}%，明度 ${Math.round(brightness)}%`"
          @pointerdown="startSaturationBrightness"
          @pointermove="event => svPanel?.hasPointerCapture(event.pointerId) && updateSaturationBrightness(event)"
          @keydown="handleSaturationBrightnessKeydown"
        >
          <span
            class="absolute z-1 size-3 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-white shadow-[0_1px_3px_#00000080]"
            :style="svCursorStyle"
          />
        </div>

        <div
          ref="hueSlider"
          class="base-color-picker__hue relative touch-none rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
          role="slider"
          tabindex="0"
          aria-label="色相"
          aria-orientation="vertical"
          aria-valuemin="0"
          aria-valuemax="360"
          :aria-valuenow="Math.round(hue)"
          @pointerdown="startHue"
          @pointermove="event => hueSlider?.hasPointerCapture(event.pointerId) && updateHue(event)"
          @keydown="handleHueKeydown"
        >
          <span
            class="absolute left-1/2 h-1.5 w-4.5 -translate-x-1/2 -translate-y-1/2 rounded-xs bg-white shadow-[0_0_0_1px_#64748b,0_1px_2px_#0000004d]"
            :style="hueThumbStyle"
          />
        </div>
      </div>

      <div v-if="props.allowAlpha" class="grid grid-cols-[1fr_36px] items-center gap-2">
        <div
          ref="alphaSlider"
          class="base-color-picker__alpha relative h-3 touch-none rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
          :style="alphaTrackStyle"
          role="slider"
          tabindex="0"
          aria-label="不透明度"
          aria-orientation="horizontal"
          aria-valuemin="0"
          aria-valuemax="100"
          :aria-valuenow="Math.round(alpha * 100)"
          :aria-valuetext="`不透明度 ${Math.round(alpha * 100)}%`"
          @pointerdown="startAlpha"
          @pointermove="event => alphaSlider?.hasPointerCapture(event.pointerId) && updateAlpha(event)"
          @keydown="handleAlphaKeydown"
        >
          <span
            class="absolute top-1/2 size-3.5 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-white shadow-[0_1px_3px_#00000080]"
            :style="alphaThumbStyle"
          />
        </div>
        <span class="text-right font-mono text-cp-xs tabular-nums text-cp-text-secondary">
          {{ Math.round(alpha * 100) }}%
        </span>
      </div>

      <div v-if="props.presets.length > 0" class="flex flex-wrap gap-2" role="radiogroup" aria-label="预设颜色">
        <button
          v-for="color in props.presets"
          :key="color"
          type="button"
          class="relative flex size-6.5 items-center justify-center rounded-sm border-0 outline-none transition-transform duration-150 hover:scale-105 focus-visible:ring-2 focus-visible:ring-cp-control-outline focus-visible:ring-offset-2 focus-visible:ring-offset-cp-bg-container"
          :style="{ backgroundColor: color }"
          role="radio"
          :aria-label="color"
          :aria-checked="isPresetSelected(color)"
          :title="color"
          @click="choosePreset(color)"
        >
          <Check
            v-if="isPresetSelected(color)"
            class="size-3.5 drop-shadow-[0_1px_2px_rgb(0_0_0/0.2)]"
            :class="presetCheckClass(color)"
          />
        </button>
      </div>

      <div class="flex items-start gap-2">
        <div class="min-w-0 flex-1">
          <BaseInput
            v-model="customInput"
            class="rounded-cp bg-cp-fill-tertiary!"
            :aria-label="props.allowAlpha ? 'RGBA 颜色' : 'HEX 颜色'"
            :placeholder="props.allowAlpha ? 'rgba(96, 165, 250, 1)' : '#2563EB'"
            autocomplete="off"
            :disabled="props.disabled"
            :aria-invalid="Boolean(inputError) || undefined"
            aria-describedby="color-input-error"
            @change="updateFromInput"
            @keydown.enter="confirm"
          />
          <p
            v-if="inputError"
            id="color-input-error"
            class="mt-1.5 mb-0 text-xs font-emphasis text-cp-error-text"
            aria-live="polite"
          >
            {{ inputError }}
          </p>
        </div>
        <BaseButton size="md" variant="secondary" :disabled="!inputValid" @click="confirm">
          确定
        </BaseButton>
      </div>
    </div>
  </BasePopover>
</template>

<style scoped>
.base-color-picker__sv::before,
.base-color-picker__sv::after {
  position: absolute;
  inset: 0;
  content: '';
}

.base-color-picker__sv::before {
  background: linear-gradient(to right, #fff, transparent);
}

.base-color-picker__sv::after {
  background: linear-gradient(to top, #000, transparent);
}

.base-color-picker__hue {
  background: linear-gradient(
    to bottom,
    #f00 0%,
    #ff0 16.67%,
    #0f0 33.33%,
    #0ff 50%,
    #00f 66.67%,
    #f0f 83.33%,
    #f00 100%
  );
}
</style>
