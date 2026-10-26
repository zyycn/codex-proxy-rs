<script setup lang="ts">
import { ArrowInDownSquareHalf } from '@boxicons/vue'

import { ZCheckbox, ZConfirmDialog, ZIconButton, ZPageHeader, ZSegmented } from '@codex-proxy/ui'

import { PackageOpen, PanelsTopLeft, RefreshCw } from '@lucide/vue'
import { computed, shallowRef } from 'vue'

import { useI18n } from 'vue-i18n'
import { usePluginManagement } from '../composables/usePluginManagement'
import PluginCatalogPanel from './PluginCatalogPanel.vue'
import PluginConfigurationModal from './PluginConfigurationModal.vue'
import PluginDetailModal from './PluginDetailModal.vue'
import PluginEnableModal from './PluginEnableModal.vue'
import PluginExtensionsPanel from './PluginExtensionsPanel.vue'
import PluginHelpPopover from './PluginHelpPopover.vue'
import PluginInstallModal from './PluginInstallModal.vue'
import PluginRollbackModal from './PluginRollbackModal.vue'
import PluginUpdateCheckModal from './PluginUpdateCheckModal.vue'

const { t } = useI18n({ useScope: 'global' })

const activeTab = shallowRef<'artifacts' | 'extensions'>('artifacts')
const management = usePluginManagement()

const tabOptions = computed(() => ([
  {
    label: t('plugins.management.installed'),
    value: 'artifacts',
    icon: PackageOpen,
  },
  {
    label: t('plugins.management.extensions'),
    value: 'extensions',
    icon: PanelsTopLeft,
  },
]))
</script>

<template>
  <div class="flex h-[calc(100dvh-2rem)] min-h-0 w-full flex-none! flex-col gap-3 overflow-hidden min-[961px]:h-[calc(100dvh-3rem)]">
    <ZPageHeader :title="t('navigation.plugins')" :description="t('plugins.management.description')" />

    <div class="flex shrink-0 items-start gap-3">
      <ZSegmented
        v-model="activeTab"
        :options="tabOptions"
        :aria-label="t('plugins.management.tabsLabel')"
      />
      <div class="flex shrink-0 items-center gap-1">
        <ZIconButton :aria-label="t('plugins.installation.installPlugin')" variant="solid" @click="management.openInstall('upload')">
          <ArrowInDownSquareHalf pack="filled" class="size-5" />
        </ZIconButton>
        <ZIconButton :aria-label="t('plugins.management.refresh')" variant="solid" :loading="management.loading.value" @click="management.refresh()">
          <template #loading>
            <RefreshCw class="size-4.5 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-4.5" />
        </ZIconButton>
      </div>
    </div>

    <div class="flex min-h-0 flex-1 flex-col gap-3">
      <PluginCatalogPanel
        v-if="activeTab === 'artifacts'"
        class="min-h-0 flex-1"
        :plugins="management.catalog.value"
        :loading="management.loading.value"
        @manage="management.openDetail($event.id)"
      />
      <PluginExtensionsPanel
        v-else
        class="min-h-0 flex-1"
        :views="management.extensions.value"
        :loading="management.loading.value"
      />
    </div>

    <PluginDetailModal
      :model-value="management.showDetail.value && !management.updateCheck.open.value"
      :plugin="management.selectedPlugin.value"
      :initial-section="management.detailSection.value"
      :views="management.extensions.value"
      :busy="management.savingInstance.value || management.uninstall.busy.value || Boolean(management.busyInstanceId.value || management.busyDigest.value)"
      @update:model-value="management.showDetail.value = $event"
      @accept="management.openAcceptance"
      @edit="management.openEditInstance"
      @enable="management.requestInstanceEnable"
      @disable="management.requestInstanceDisable"
      @delete-configuration="management.requestInstanceDelete"
      @delete-version="management.requestArtifactDelete"
      @rollback="management.openRollback"
      @install-version="management.openVersionInstall"
      @check-update="management.updateCheck.check"
      @switch-version="management.requestVersionSwitch"
      @uninstall="management.uninstall.request"
    />

    <PluginInstallModal
      v-model="management.showInstall.value"
      :mode="management.installMode.value"
      :installed-artifacts="management.artifacts.value"
      :acceptance-artifact="management.acceptanceArtifact.value"
      :credentials="management.credentials.value"
      :saving-credential="management.savingCredential.value"
      :save-credential="management.saveCredential"
      :release="management.release.value"
      :update-source="management.updateSource.value"
      :update-selection="management.updateSelection.value"
      :save-source="management.saveInstallSource"
      :saving="management.installing.value"
      :querying="management.queryingRelease.value"
      :verifying="management.verifyingArtifact.value"
      :verified="management.verifiedArtifact.value"
      @install="management.installArtifact"
      @accept="management.acceptArtifact"
      @query="management.queryRelease"
      @verify="management.verifyArtifact"
      @reset-verification="management.resetArtifactVerification"
      @reset-query="management.resetReleaseQuery"
      @change-mode="management.changeInstallMode"
      @delete-credential="management.requestCredentialDelete"
    />

    <PluginUpdateCheckModal
      v-model="management.updateCheck.open.value"
      :plugin="management.updateCheck.plugin.value"
      :result="management.updateCheck.result.value"
      :checking="management.updateCheck.checking.value"
      :upgrading="management.installing.value"
      @install="management.updateCheck.open.value = false; management.openCheckedUpdate($event)"
      @upgrade="management.upgradeCheckedPlugin"
    />

    <PluginConfigurationModal
      v-model="management.showInstance.value"
      :instance="management.editingInstance.value"
      :artifact="management.configurationArtifact.value"
      :draft="management.configurationDraft.value"
      :error="management.configurationError.value"
      :saving="management.savingInstance.value"
      @save="management.saveInstance"
    />

    <ZConfirmDialog
      v-model="management.showVersionSwitch.value"
      :title="t('plugins.management.switchPluginVersion')"
      :description="t('plugins.management.switchHelp')"
      :confirm-text="t('plugins.enable.confirmSwitch')"
      :loading="Boolean(management.busyInstanceId.value)"
      @confirm="management.confirmVersionSwitch"
    >
      <p class="m-0 text-cp-sm">
        {{ management.pendingVersionSwitch.value?.artifact.metadata.displayName }}：
        {{ management.pendingVersionSwitch.value?.currentVersion }} → {{ management.pendingVersionSwitch.value?.artifact.metadata.version }}
      </p>
    </ZConfirmDialog>

    <PluginRollbackModal
      v-model="management.showRollback.value"
      :instance="management.rollbackInstance.value"
      :plan="management.rollbackPlan.value"
      :loading="management.loadingRollback.value"
      :saving="Boolean(management.busyInstanceId.value)"
      @confirm="management.confirmRollback"
      @reload="management.openRollback"
    />

    <PluginEnableModal
      v-model="management.showEnableConfirmation.value"
      :request="management.pendingEnable.value?.request"
      :metadata="management.pendingEnable.value?.artifact.metadata"
      :replacements="management.pendingEnable.value?.replacements ?? []"
      :saving="management.savingInstance.value"
      @confirm="management.confirmInstanceEnable"
      @closed="management.pendingEnable.value = null"
    />

    <ZConfirmDialog
      v-model="management.showArtifactDelete.value"
      :title="t('plugins.management.deletePluginVersion')"
      :description="t('plugins.management.deleteVersionHelp')"
      destructive
      :confirm-text="t('common.actions.delete')"
      :loading="Boolean(management.busyDigest.value)"
      @confirm="management.confirmArtifactDelete"
    >
      <div class="flex min-w-0 items-center gap-2 text-cp-sm font-normal">
        <p class="m-0 min-w-0 wrap-anywhere">
          {{ t('plugins.management.delete', { displayName: management.pendingArtifact.value?.metadata.displayName, version: management.pendingArtifact.value?.metadata.version }) }}
        </p>
        <PluginHelpPopover :label="t('plugins.management.versionDeletionDetails')">
          <p class="m-0">
            {{ t('plugins.management.deleteVersionInUse') }}
          </p>
          <p class="m-0">
            {{ t('plugins.management.deleteVersionCleanupHelp') }}
          </p>
          <p class="m-0">
            SHA-256
          </p>
          <p class="m-0 break-all font-mono">
            {{ management.pendingArtifact.value?.metadata.sha256 }}
          </p>
        </PluginHelpPopover>
      </div>
      <p class="mt-3 mb-0 text-cp-xs font-normal text-cp-text-secondary">
        {{ t('plugins.management.deleteVersionCleanupLabel') }}
      </p>
    </ZConfirmDialog>

    <ZConfirmDialog
      v-model="management.showInstanceDelete.value"
      :title="t('plugins.management.deletePluginConfiguration')"
      :description="t('plugins.management.deleteConfigurationWarning')"
      destructive
      :confirm-text="t('common.actions.delete')"
      :loading="Boolean(management.busyInstanceId.value)"
      @confirm="management.confirmInstanceDelete"
    >
      <div class="grid gap-3 text-cp-sm font-normal">
        <p class="m-0 wrap-anywhere">
          {{ management.pendingDeleteInstance.value?.name }}
        </p>
        <p class="m-0 text-cp-xs">
          {{ t('plugins.management.deleteConfigurationHelp') }}
        </p>
      </div>
    </ZConfirmDialog>

    <ZConfirmDialog
      v-model="management.uninstall.open.value"
      :title="t('plugins.details.uninstallPlugin')"
      :description="t('plugins.management.uninstallWarning')"
      destructive
      :confirm-text="t('plugins.management.confirmUninstall')"
      :loading="management.uninstall.busy.value"
      :confirm-disabled="!management.uninstall.acknowledged.value"
      @confirm="management.uninstall.confirm"
    >
      <div class="grid gap-3 text-cp-sm font-normal">
        <p class="m-0 wrap-anywhere">
          {{ t('plugins.management.versionCount', { displayName: management.uninstall.pending.value?.artifact.metadata.displayName, count: management.uninstall.pending.value?.artifacts.length }) }}
        </p>
        <ul v-if="management.uninstall.pending.value?.configurations.length" class="m-0 max-h-40 space-y-1 overflow-y-auto pl-4">
          <li v-for="instance in management.uninstall.pending.value.configurations" :key="instance.id" class="wrap-anywhere">
            {{ instance.name }}
          </li>
        </ul>
        <div class="flex items-center gap-1.5 text-cp-xs text-cp-text-secondary">
          <span>{{ t('plugins.management.uninstallCleanupLabel') }}</span>
          <PluginHelpPopover :label="t('plugins.management.uninstallCleanupInstructions')">
            <p class="m-0">
              {{ t('plugins.management.uninstallCleanupHelp') }}
            </p>
            <p class="m-0">
              {{ t('plugins.management.disableInsteadHelp') }}
            </p>
          </PluginHelpPopover>
        </div>
        <ZCheckbox v-model="management.uninstall.acknowledged.value" :label="t('plugins.management.permanentDeleteLabel')" :disabled="management.uninstall.busy.value" />
        <p v-if="management.uninstall.progress.value" role="status" class="m-0 text-cp-xs text-cp-text-secondary">
          {{ management.uninstall.progress.value }}
        </p>
      </div>
    </ZConfirmDialog>

    <ZConfirmDialog
      v-model="management.showCredentialDelete.value"
      :title="t('plugins.management.deleteDownloadCredentials')"
      :description="t('plugins.management.itCannotBeDeletedWhileItIs')"
      destructive
      :confirm-text="t('common.actions.delete')"
      :loading="Boolean(management.busyCredentialId.value)"
      @confirm="management.confirmCredentialDelete"
    >
      <p class="m-0 wrap-anywhere text-cp-sm font-normal">
        {{ management.pendingCredential.value?.name }}
      </p>
    </ZConfirmDialog>
  </div>
</template>
