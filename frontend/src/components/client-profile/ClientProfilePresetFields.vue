<script setup lang="ts">
import type { ClientProfilePreset, ClientProfilePreview, ClientProfileSelection, CustomClientProfileSelection, PresetClientProfileSelection } from '@/api/modules/client-profiles'
import { BaseFormItem, BaseInput, BaseSelect, BaseTextarea } from '@codex-proxy/ui'
import { computed, shallowRef } from 'vue'

const props = withDefaults(defineProps<{
  presets: ClientProfilePreset[]
  preview?: ClientProfilePreview
  previewing: boolean
  error: string
  disabled: boolean
  maxVersionLag?: number
}>(), {
  maxVersionLag: 10,
})
const model = defineModel<ClientProfileSelection | null>({ required: true })
const presetDraft = shallowRef<PresetClientProfileSelection>()
const customDraft = shallowRef<CustomClientProfileSelection>()
const preset = computed(() => model.value && !model.value.mode ? model.value : undefined)
const custom = computed(() => model.value?.mode === 'custom' || preset.value?.versionMode === 'fixed')
const platforms = { macos: 'MacOS', linux: 'Linux', windows: 'Windows' }
const presetOptions = computed(() => props.presets.map(({ configuration }) => ({
  value: `${configuration.platform}-${configuration.client}`,
  label: `${platforms[configuration.platform]} · ${configuration.client === 'desktop' ? 'Desktop' : 'CLI'}`,
})))
const currentPreset = computed(() => props.presets.find(({ configuration }) =>
  configuration.client === preset.value?.client && configuration.platform === preset.value?.platform,
))
const selectedPreset = computed({
  get: () => {
    const selection = preset.value ?? presetDraft.value
    return selection ? `${selection.platform}-${selection.client}` : ''
  },
  set: (value: string) => {
    const selected = props.presets.find(({ configuration }) => `${configuration.platform}-${configuration.client}` === value)
    if (selected) {
      customDraft.value = undefined
      model.value = { ...selected.configuration, versionMode: selected.automaticAvailable ? 'latest' : 'fixed' }
    }
  },
})
const cliEntry = computed({
  get: () => preset.value?.cliEntry ?? 'core',
  set: (value: string) => {
    if (preset.value && (value === 'core' || value === 'tui' || value === 'exec')) {
      customDraft.value = undefined
      model.value = { ...preset.value, cliEntry: value === 'core' ? null : value }
    }
  },
})
const versionMode = computed({
  get: () => custom.value ? 'custom' : 'latest',
  set: (value: string) => {
    if (value === 'custom') {
      if (!props.preview && !customDraft.value)
        return
      if (preset.value)
        presetDraft.value = { ...preset.value }
      model.value = { ...(customDraft.value ?? { mode: 'custom', userAgent: props.preview!.userAgent }) }
    }
    else {
      if (model.value?.mode === 'custom')
        customDraft.value = { ...model.value }
      const selection = preset.value ?? presetDraft.value ?? props.presets[0]?.configuration
      if (selection) {
        model.value = { ...selection, versionMode: 'latest', codexVersion: null, desktopVersion: null, desktopBuild: null }
      }
    }
  },
})
const versionLag = computed({
  get: () => preset.value?.versionLag == null ? '' : String(preset.value.versionLag),
  set: (value: string) => {
    if (preset.value)
      model.value = { ...preset.value, versionLag: value === '' ? null : Number(value) }
  },
})
const versionLagError = computed(() => {
  const value = preset.value?.versionLag
  return value != null && (!Number.isInteger(value) || value < 1 || value > props.maxVersionLag)
    ? `请输入 1～${props.maxVersionLag} 的正整数`
    : ''
})
const terminal = computed({
  get: () => preset.value?.terminal ?? '',
  set: (value: string) => {
    if (preset.value)
      model.value = { ...preset.value, terminal: value === '' ? null : value }
  },
})
const userAgent = computed({
  get: () => model.value?.mode === 'custom' ? model.value.userAgent : props.preview?.userAgent ?? '',
  set: (value: string) => {
    const previous = model.value?.mode === 'custom' ? model.value : undefined
    if (preset.value)
      presetDraft.value = { ...preset.value }
    // 已知客户端的配套头重新派生，避免修改版本后继续携带旧值。
    const recognized = /^(?:Codex Desktop|codex-tui|codex_cli_rs|codex_exec)\/\S+(?:\s|$)/.test(value)
    model.value = {
      mode: 'custom',
      userAgent: value,
      ...(!recognized && previous ? { originator: previous.originator, codexVersion: previous.codexVersion } : {}),
    }
  },
})
</script>

<template>
  <div class="grid gap-4">
    <div class="grid gap-4" :class="preset?.client === 'cli' && !custom ? 'sm:grid-cols-3' : 'sm:grid-cols-2'">
      <BaseFormItem label="客户端预设">
        <BaseSelect v-model="selectedPreset" class="w-full" :options="presetOptions" :placeholder="custom ? '使用自定义用户代理' : undefined" :disabled="disabled || custom || !presets.length" />
      </BaseFormItem>
      <BaseFormItem v-if="preset?.client === 'cli' && !custom" label="CLI 入口">
        <BaseSelect
          v-model="cliEntry"
          class="w-full"
          :options="[
            { label: 'Core · 默认身份', value: 'core' },
            { label: 'TUI · 交互式终端', value: 'tui' },
            { label: 'Exec · 非交互执行', value: 'exec' },
          ]"
          :disabled="disabled"
        />
      </BaseFormItem>
      <BaseFormItem label="版本策略">
        <BaseSelect
          v-model="versionMode"
          class="w-full"
          :options="[
            { label: '跟随最新版本', value: 'latest', disabled: !presets.length },
            { label: '自定义版本', value: 'custom', disabled: !custom && (previewing || !preview) },
          ]"
          :disabled="disabled"
        />
      </BaseFormItem>
    </div>
    <p v-if="!custom && currentPreset?.reason" class="m-0 text-cp-sm text-cp-text-secondary">
      {{ currentPreset.reason }}
    </p>
    <div v-if="!custom" class="grid gap-4 sm:grid-cols-2">
      <BaseFormItem label="版本滞后" :error="versionLagError">
        <BaseInput
          v-model="versionLag"
          aria-label="版本滞后"
          type="number"
          inputmode="numeric"
          min="1"
          :max="maxVersionLag"
          step="1"
          :placeholder="`滞后 1～${maxVersionLag} 个版本，留空不滞后`"
          :disabled="disabled"
        />
      </BaseFormItem>
      <BaseFormItem label="终端标识">
        <BaseInput
          v-model="terminal"
          :disabled="disabled"
          aria-label="终端标识"
          placeholder="留空使用 unknown"
          maxlength="128"
          spellcheck="false"
          autocomplete="off"
          class="font-mono"
        />
      </BaseFormItem>
    </div>
    <template v-if="custom">
      <BaseTextarea
        v-model="userAgent"
        :rows="3"
        :disabled="disabled || (!model?.mode && previewing)"
        aria-label="用户代理"
        placeholder="填写完整用户代理"
        spellcheck="false"
        autocomplete="off"
        class="font-mono"
      />
      <p v-if="error" role="alert" class="m-0 text-cp-sm text-cp-error">
        {{ error }}
      </p>
    </template>
  </div>
</template>
