<script setup lang="ts">
import { Github } from '@boxicons/vue'
import { ZDialog, ZMenuItem } from '@codex-proxy/ui'
import { AppWindow } from '@lucide/vue'
import { useI18n } from 'vue-i18n'

const emit = defineEmits<{
  select: []
  afterLeave: []
}>()
const open = defineModel<boolean>({ default: false })
const { t } = useI18n({ useScope: 'global' })

function select() {
  emit('select')
  open.value = false
}
</script>

<template>
  <ZDialog
    v-model="open"
    :title="t('keys.configuration.importTo')"
    :description="t('keys.configuration.selectImportTool')"
    width="24rem"
    @closed="emit('afterLeave')"
  >
    <div class="grid gap-2">
      <div
        class="flex items-center gap-1 rounded-z-sm pr-1 transition-colors hover:bg-z-bg-text-hover has-[:focus-visible]:bg-z-bg-text-hover motion-reduce:transition-none"
      >
        <ZMenuItem class="min-h-18 min-w-0 py-3 hover:bg-transparent!" @click="select">
          <template #icon>
            <span class="grid size-9 place-items-center rounded-z bg-z-fill-quaternary text-z-text-secondary">
              <AppWindow class="size-5" aria-hidden="true" />
            </span>
          </template>
          <span class="grid gap-1 whitespace-normal">
            <span>CC Switch</span>
            <span class="text-xs leading-5 font-normal text-z-text-secondary">{{ t('keys.configuration.ccSwitchDescription') }}</span>
          </span>
        </ZMenuItem>
        <a
          href="https://github.com/farion1231/cc-switch"
          target="_blank"
          rel="noopener noreferrer"
          :aria-label="t('keys.configuration.viewOnGitHub', { name: 'CC Switch' })"
          class="inline-grid size-9 shrink-0 place-items-center rounded-z-sm text-z-text-tertiary outline-none transition-colors hover:text-z-text focus-visible:ring-2 focus-visible:ring-z-control-outline focus-visible:ring-inset motion-reduce:transition-none"
        >
          <Github class="size-6" aria-hidden="true" />
        </a>
      </div>
    </div>
  </ZDialog>
</template>
