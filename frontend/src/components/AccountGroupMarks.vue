<script setup lang="ts">
import type { AccountGroupRef } from '@/api'

import { ZIconButton, ZPopover } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  groups: AccountGroupRef[]
}>()

const { t } = useI18n({ useScope: 'global' })

const groupMarks = computed(() =>
  props.groups.map(group => ({
    ...group,
    label: group.enabled
      ? group.name
      : t('common.status.disabledName', { name: group.name }),
  })),
)
</script>

<template>
  <div
    v-if="groups.length > 0"
    class="flex min-w-0 flex-wrap items-center gap-1.5"
  >
    <ZPopover
      v-for="group in groupMarks"
      :key="group.id"
      class="shrink-0"
      trigger="hover-click"
      placement="top"
      role="tooltip"
      :hover-delay="240"
    >
      <template #reference="{ open, id }">
        <ZIconButton
          variant="link"
          size="small"
          class="size-4! rounded-sm!"
          :aria-label="group.label"
          :aria-expanded="open"
          :aria-describedby="open ? id : undefined"
          :title="undefined"
        >
          <span class="size-4 rounded-sm" :style="{ backgroundColor: group.color }" aria-hidden="true" />
        </ZIconButton>
      </template>
      <div class="max-w-72 px-3 py-2 text-z-xs wrap-anywhere">
        {{ group.label }}
      </div>
    </ZPopover>
  </div>
  <span v-else class="text-xs text-z-text-quaternary">—</span>
</template>
