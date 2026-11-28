<script setup lang="ts">
import type { ProfileOptions, ProfilePreview, ProfileSelection } from './profile'
import type { ProfileEditorProps } from '@/components/request-profile/useProfileEditor'
import { ZFormItem, ZInput, ZSelect } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import ProfileEditorFrame from '@/components/request-profile/ProfileEditorFrame.vue'
import { useProfileEditor } from '@/components/request-profile/useProfileEditor'

const props = defineProps<ProfileEditorProps<ProfileSelection>>()
const model = defineModel<ProfileSelection | null | undefined>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const { options, preview, effective, source, frame, loading } = useProfileEditor<ProfileSelection, ProfileOptions, ProfilePreview>(
  'antigravity',
  model,
  props,
  selection => selection.versionMode === 'fixed' && !selection.clientVersion,
)
const platforms = { macos: 'MacOS', linux: 'Linux', windows: 'Windows' }
const presetOptions = computed(() => (options.value?.presets ?? []).map(({ configuration }) => ({
  label: `${platforms[configuration.platform]} · ${configuration.client === 'desktop'
    ? 'Desktop'
    : 'CLI'}`,
  value: `${configuration.platform}-${configuration.client}`,
})))
const currentPreset = computed(() => options.value?.presets.find(({ configuration }) =>
  configuration.platform === effective.value?.platform && configuration.client === effective.value?.client))
const selectedPreset = computed({
  get: () => effective.value
    ? `${effective.value.platform}-${effective.value.client}`
    : undefined,
  set: (value: string) => {
    const preset = options.value?.presets.find(({ configuration }) => `${configuration.platform}-${configuration.client}` === value)
    if (preset) {
      model.value = {
        ...preset.configuration,
        systemPrompt: effective.value?.systemPrompt ?? null,
        versionMode: preset.automaticAvailable ? 'latest' : 'fixed',
      }
    }
  },
})
const versionMode = computed({
  get: () => effective.value?.versionMode ?? 'latest',
  set: (value: string) => {
    if (effective.value) {
      model.value = {
        ...effective.value,
        versionMode: value === 'fixed'
          ? 'fixed'
          : 'latest',
        clientVersion: value === 'fixed'
          ? preview.value?.clientVersion ?? null
          : null,
      }
    }
  },
})

function updateVersion(value: string) {
  if (effective.value)
    model.value = { ...effective.value, clientVersion: value.trim() || null }
}
</script>

<template>
  <ProfileEditorFrame v-model="source" v-bind="frame">
    <template #source-extra>
      <slot name="source-extra" />
    </template>
    <div class="grid gap-4 sm:grid-cols-2">
      <ZFormItem :label="t('settings.clientProfile.clientDefault')">
        <ZSelect v-model="selectedPreset" class="w-full" :options="presetOptions" :disabled="disabled || loading || !presetOptions.length" />
      </ZFormItem>
      <ZFormItem :label="t('settings.clientProfile.versionStrategy')">
        <ZSelect
          v-model="versionMode"
          class="w-full"
          :disabled="disabled || loading"
          :options="[
            { label: t('settings.clientProfile.followLatestVersion'), value: 'latest', disabled: !currentPreset?.automaticAvailable },
            { label: t('settings.clientProfile.customVersion'), value: 'fixed' },
          ]"
        />
      </ZFormItem>
    </div>
    <p v-if="currentPreset?.reason" class="m-0 text-z-sm text-z-text-secondary">
      {{ currentPreset.reason }}
    </p>
    <div v-if="effective?.versionMode === 'fixed'" class="grid gap-4 sm:grid-cols-2">
      <ZFormItem :label="t('settings.clientProfile.clientVersion')" required>
        <ZInput
          :model-value="effective?.clientVersion ?? ''"
          :disabled="disabled || loading"
          :placeholder="t('settings.clientProfile.forExample')"
          @update:model-value="updateVersion($event)"
        />
      </ZFormItem>
    </div>
  </ProfileEditorFrame>
</template>
