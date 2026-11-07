<script setup lang="ts">
import type { AccountAuthorizationView } from '../../composables/useAccountAuthorization'

import { ZButton, ZForm, ZFormItem, ZIconButton, ZScrollbar, ZTextarea } from '@codex-proxy/ui'
import { Copy, KeyRound } from '@lucide/vue'
import { useI18n } from 'vue-i18n'
import { useCopyText } from '@/composables/useCopyText'

defineProps<{
  authUrl: string
  panelTitle: string
  panelDescription: string
  callbackLabel: string
  callbackPlaceholder: string
  loading: boolean
  disabled: boolean
  authorization: AccountAuthorizationView
  canStart: boolean
}>()

const emit = defineEmits<{ regenerate: [] }>()

const { t } = useI18n({ useScope: 'global' })

const callback = defineModel<string>({ required: true })
const copyWithToast = useCopyText()
</script>

<template>
  <div class="flex flex-col gap-4">
    <div class="rounded-z bg-z-fill-quaternary px-4 py-3">
      <div class="flex items-start gap-3">
        <div
          class="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-z bg-z-bg-container text-z-primary-text"
        >
          <KeyRound class="size-4" />
        </div>
        <div class="min-w-0 flex-1">
          <p class="m-0 text-z font-bold text-z-text">
            {{ panelTitle }}
          </p>
          <p class="m-0 mt-1 text-z-sm leading-[1.55] font-medium text-z-text-secondary">
            {{ panelDescription }}
          </p>
        </div>
      </div>
    </div>

    <div class="flex flex-wrap items-center gap-2">
      <ZButton
        :loading="loading"
        :disabled="disabled || (!authorization.flow && !canStart)"
        @click="emit('regenerate')"
      >
        {{ authUrl ? t('accounts.onboarding.regenerateAuthorizationLink') : t('accounts.onboarding.generateAuthorizationLink') }}
      </ZButton>
    </div>

    <ZForm v-if="authUrl">
      <ZFormItem :label="t('accounts.onboarding.authorizationLink')">
        <template #extra>
          <ZIconButton
            variant="solid"
            size="small"
            :aria-label="t('accounts.onboarding.copyLink')"
            :disabled="disabled"
            @click="copyWithToast(authUrl, { successText: t('accounts.onboarding.authorizationLinkCopied') })"
          >
            <Copy class="size-3.5" />
          </ZIconButton>
        </template>
        <ZScrollbar max-height="92px">
          <div class="rounded-z bg-(--z-input-bg) px-3.5 py-3 shadow-z-tertiary">
            <pre
              class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-z-sm leading-[1.6] font-emphasis text-z-text-secondary"
              v-text="authUrl"
            />
          </div>
        </ZScrollbar>
      </ZFormItem>
    </ZForm>

    <ZForm v-if="authorization.flow">
      <ZFormItem :label="callbackLabel" required>
        <ZTextarea
          v-model="callback"
          :aria-label="callbackLabel"
          :rows="4"
          :placeholder="callbackPlaceholder"
          :disabled="disabled"
        />
      </ZFormItem>
    </ZForm>
  </div>
</template>
