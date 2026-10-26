<script setup lang="ts">
import type { ClientProfilePreset, ClientProfilePreview, ClientProfileSelection, CustomClientProfileSelection, PresetClientProfileSelection } from '@/api/modules/settings/profiles'

import { ZFormItem, ZInput, ZSelect, ZTextarea } from '@codex-proxy/ui'
import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'

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

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<ClientProfileSelection | null>({ required: true })
const presetDraft = shallowRef<PresetClientProfileSelection>()
const customDraft = shallowRef<CustomClientProfileSelection>()
const preset = computed(() => model.value && !model.value.mode
  ? model.value
  : undefined)
const custom = computed(() => model.value?.mode === 'custom' || preset.value?.versionMode === 'fixed')
const platforms = { macos: 'MacOS', linux: 'Linux', windows: 'Windows' }
const presetOptions = computed(() => props.presets.map(({ configuration }) => ({
  value: `${configuration.platform}-${configuration.client}`,
  label: `${platforms[configuration.platform]} · ${configuration.client === 'desktop'
    ? 'Desktop'
    : 'CLI'}`,
})))
const currentPreset = computed(() => props.presets.find(({ configuration }) =>
  configuration.client === preset.value?.client && configuration.platform === preset.value?.platform,
))
const selectedPreset = computed({
  get: () => {
    const selection = preset.value ?? presetDraft.value
    return selection
      ? `${selection.platform}-${selection.client}`
      : ''
  },
  set: (value: string) => {
    const selected = props.presets.find(({ configuration }) => `${configuration.platform}-${configuration.client}` === value)
    if (selected) {
      customDraft.value = undefined
      model.value = {
        ...selected.configuration,
        versionMode: selected.automaticAvailable ? 'latest' : 'fixed',
      }
    }
  },
})
const cliEntry = computed({
  get: () => preset.value?.cliEntry ?? 'core',
  set: (value: string) => {
    if (preset.value && (value === 'core' || value === 'tui' || value === 'exec')) {
      customDraft.value = undefined
      model.value = {
        ...preset.value,
        cliEntry: value === 'core'
          ? null
          : value,
      }
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
      model.value = {
        ...(customDraft.value ?? { mode: 'custom', userAgent: props.preview!.userAgent }),
      }
    }
    else {
      if (model.value?.mode === 'custom')
        customDraft.value = { ...model.value }
      const selection = preset.value ?? presetDraft.value ?? props.presets[0]?.configuration
      if (selection) {
        model.value = {
          ...selection,
          versionMode: 'latest',
          codexVersion: null,
          desktopVersion: null,
          desktopBuild: null,
        }
      }
    }
  },
})
const versionLag = computed({
  get: () => preset.value?.versionLag == null
    ? ''
    : String(preset.value.versionLag),
  set: (value: string) => {
    if (preset.value) {
      model.value = {
        ...preset.value,
        versionLag: value === ''
          ? null
          : Number(value),
      }
    }
  },
})
const versionLagError = computed(() => {
  const value = preset.value?.versionLag
  return value != null && (!Number.isInteger(value) || value < 1 || value > props.maxVersionLag)
    ? t('settings.clientProfile.enterIntegerFromTo', { version: props.maxVersionLag })
    : ''
})
const terminal = computed({
  get: () => preset.value?.terminal ?? '',
  set: (value: string) => {
    if (preset.value) {
      model.value = {
        ...preset.value,
        terminal: value === ''
          ? null
          : value,
      }
    }
  },
})
const osVersion = computed({
  get: () => preset.value?.osVersion ?? '',
  set: (value: string) => {
    if (preset.value) {
      model.value = {
        ...preset.value,
        osVersion: value === ''
          ? null
          : value,
      }
    }
  },
})
const userAgent = computed({
  get: () => model.value?.mode === 'custom'
    ? model.value.userAgent
    : props.preview?.userAgent ?? '',
  set: (value: string) => {
    const previous = model.value?.mode === 'custom'
      ? model.value
      : undefined
    if (preset.value)
      presetDraft.value = { ...preset.value }
    // 已知客户端的配套头重新派生，避免修改版本后继续携带旧值。
    const recognized = /^(?:Codex Desktop|codex-tui|codex_cli_rs|codex_exec)\/\S+(?:\s|$)/.test(value)
    model.value = {
      mode: 'custom',
      userAgent: value,
      ...(!recognized && previous
        ? { originator: previous.originator, codexVersion: previous.codexVersion }
        : {}),
    }
  },
})
</script>

<template>
  <div class="grid gap-4">
    <div class="grid gap-4" :class="preset?.client === 'cli' && !custom ? 'sm:grid-cols-3' : 'sm:grid-cols-2'">
      <ZFormItem :label="t('settings.clientProfile.clientDefault')">
        <ZSelect v-model="selectedPreset" class="w-full" :options="presetOptions" :placeholder="custom ? t('settings.clientProfile.useCustomUserAgent') : undefined" :disabled="disabled || custom || !presets.length" />
      </ZFormItem>
      <ZFormItem v-if="preset?.client === 'cli' && !custom" :label="t('settings.clientProfile.cliEntry')">
        <ZSelect
          v-model="cliEntry"
          class="w-full"
          :options="[
            { label: t('settings.clientProfile.coreDefaultIdentity'), value: 'core' },
            { label: t('settings.clientProfile.tuiInteractiveTerminal'), value: 'tui' },
            { label: t('settings.clientProfile.execNonInteractiveExecution'), value: 'exec' },
          ]"
          :disabled="disabled"
        />
      </ZFormItem>
      <ZFormItem :label="t('settings.clientProfile.versionStrategy')">
        <ZSelect
          v-model="versionMode"
          class="w-full"
          :options="[
            { label: t('settings.clientProfile.followLatestVersion'), value: 'latest', disabled: !presets.length },
            { label: t('settings.clientProfile.customVersion'), value: 'custom', disabled: !custom && (previewing || !preview) },
          ]"
          :disabled="disabled"
        />
      </ZFormItem>
    </div>
    <p v-if="!custom && currentPreset?.reason" class="m-0 text-cp-sm text-cp-text-secondary">
      {{ currentPreset.reason }}
    </p>
    <div v-if="!custom" class="grid gap-4 sm:grid-cols-2">
      <ZFormItem :label="t('settings.clientProfile.versionLag')" :error="versionLagError">
        <ZInput
          v-model="versionLag"
          :aria-label="t('settings.clientProfile.versionLag')"
          type="number"
          inputmode="numeric"
          min="1"
          :max="maxVersionLag"
          step="1"
          :placeholder="t('settings.clientProfile.versionLagHelp', { version: maxVersionLag })"
          :disabled="disabled"
        />
      </ZFormItem>
      <div class="grid gap-4" :class="{ 'sm:grid-cols-2': preset?.client === 'desktop' }">
        <ZFormItem :label="t('settings.clientProfile.terminalIdentification')">
          <ZInput
            v-model="terminal"
            :disabled="disabled"
            :aria-label="t('settings.clientProfile.terminalIdentification')"
            :placeholder="t('settings.clientProfile.leaveBlankToUseUnknown')"
            maxlength="128"
            spellcheck="false"
            autocomplete="off"
            class="font-mono"
          />
        </ZFormItem>
        <ZFormItem v-if="preset?.client === 'desktop'" :label="t('settings.clientProfile.systemVersion')">
          <ZInput
            v-model="osVersion"
            :disabled="disabled"
            :aria-label="t('settings.clientProfile.systemVersion')"
            :placeholder="currentPreset?.defaults.osVersion ?? undefined"
            maxlength="128"
            spellcheck="false"
            autocomplete="off"
            class="font-mono"
          />
        </ZFormItem>
      </div>
    </div>
    <template v-if="custom">
      <ZTextarea
        v-model="userAgent"
        :rows="3"
        :disabled="disabled || (!model?.mode && previewing)"
        :aria-label="t('settings.clientProfile.userAgent')"
        :placeholder="t('settings.clientProfile.userAgentPlaceholder')"
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
