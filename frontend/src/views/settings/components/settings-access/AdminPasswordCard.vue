<script setup lang="ts">
import { ZButton, ZCard, ZMotionIcon } from '@codex-proxy/ui'

import { LockKeyhole } from '@lucide/vue'
import { shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import ChangePasswordModal from './ChangePasswordModal.vue'

const { t } = useI18n({ useScope: 'global' })

const open = shallowRef(false)
</script>

<template>
  <ZCard :title="t('settings.password.administratorPassword')" :description="t('settings.password.description')">
    <template #actions>
      <ZButton @click="open = true">
        <template #icon>
          <LockKeyhole class="size-4" aria-hidden="true" />
        </template>
        {{ t('settings.password.changePassword') }}
      </ZButton>
    </template>

    <div class="flex min-h-16 items-center gap-3 rounded-z bg-z-fill-quaternary px-4 py-3">
      <ZMotionIcon class="inline-flex size-9 shrink-0 items-center justify-center rounded-z bg-z-bg-container text-z-primary-text shadow-z-tertiary">
        <LockKeyhole class="size-4" aria-hidden="true" />
      </ZMotionIcon>
      <div class="min-w-0">
        <p class="m-0 text-z leading-[1.15] font-bold text-z-text">
          {{ t('settings.password.verifyCurrentPassword') }}
        </p>
        <p class="mt-1.5 mb-0 text-z-sm leading-snug font-emphasis text-z-text-secondary">
          {{ t('settings.password.sessionNotice') }}
        </p>
      </div>
    </div>
  </ZCard>
  <ChangePasswordModal v-model="open" />
</template>
