<script setup lang="ts">
import type { PluginInstance } from '@/api'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import PluginHelpPopover from './PluginHelpPopover.vue'

const props = defineProps<{ instance: PluginInstance }>()

const { t } = useI18n({ useScope: 'global' })

const failure = computed(() => props.instance.loadError ?? props.instance.runtime.failure?.message)
const title = computed(() => failure.value
  ? (props.instance.loadError
      ? t('plugins.status.unableToStart')
      : t('plugins.status.runFailed'))
  : props.instance.compatibilityWarning
    ? t('plugins.status.compatibilityReminder')
    : t('plugins.status.deprecatedApi'))
</script>

<template>
  <PluginHelpPopover
    v-if="failure || instance.compatibilityWarning || instance.apiDeprecations.length"
    :label="`${instance.name}：${title}`"
    :tone="failure ? 'error' : 'warning'"
  >
    <p class="m-0 font-emphasis text-cp-text">
      {{ title }}
    </p>
    <p v-if="failure" class="m-0 text-cp-error-text">
      {{ failure }}
    </p>
    <p v-if="instance.runtime.failure" class="m-0 font-mono">
      {{ instance.runtime.failure.code }}
    </p>
    <template v-if="instance.compatibilityWarning">
      <p class="m-0 text-cp-warning">
        {{ instance.compatibilityWarning }}
      </p>
      <p class="m-0">
        {{ t('plugins.status.startupUnverified') }}
      </p>
    </template>
    <div v-for="entry in instance.apiDeprecations" :key="`${entry.capability}:${entry.version}`" class="grid gap-1">
      <p class="m-0 font-mono text-cp-text">
        {{ entry.capability }} v{{ entry.version }} → v{{ entry.replacementVersion }}
      </p>
      <p class="m-0 text-cp-warning">
        {{ entry.remainingReleases > 0 ? t('plugins.status.compatibilityWindow', { remainingReleases: entry.remainingReleases }) : t('plugins.status.compatibilityEnding') }}
      </p>
      <p class="m-0">
        {{ entry.migration }}
      </p>
    </div>
    <p v-if="instance.loadError" class="m-0">
      {{ t('plugins.status.unsupportedVersionHelp') }}
    </p>
    <p v-else-if="failure" class="m-0">
      {{ t('plugins.status.restartHelp') }}
    </p>
    <p v-else-if="instance.apiDeprecations.length" class="m-0">
      {{ t('plugins.status.upgradeHelp') }}
    </p>
  </PluginHelpPopover>
</template>
