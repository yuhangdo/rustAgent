package com.yuhangdo.rustagent

import android.app.Application
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.room.Room
import com.yuhangdo.rustagent.data.local.AppDatabase
import com.yuhangdo.rustagent.data.provider.ChatProviderFactory
import com.yuhangdo.rustagent.data.provider.ChatProviderResolver
import com.yuhangdo.rustagent.data.repository.ChatRepository
import com.yuhangdo.rustagent.data.repository.RunRepository
import com.yuhangdo.rustagent.data.repository.SelectedSessionRepository
import com.yuhangdo.rustagent.data.repository.SessionRepository
import com.yuhangdo.rustagent.data.repository.SettingsRepository
import com.yuhangdo.rustagent.feature.chat.ChatViewModel
import com.yuhangdo.rustagent.feature.sessions.SessionsViewModel
import com.yuhangdo.rustagent.feature.settings.SettingsViewModel
import okhttp3.OkHttpClient

class RustAgentApp : Application() {
    val container: AppContainer by lazy {
        AppContainer(this)
    }
}

class AppContainer(
    application: Application,
) {
    private val database = Room.databaseBuilder(
        application.applicationContext,
        AppDatabase::class.java,
        "rust-agent-mobile.db",
    ).fallbackToDestructiveMigration().build()

    private val sessionRepository = SessionRepository(database.sessionDao())
    private val selectedSessionRepository = SelectedSessionRepository()
    private val settingsRepository = SettingsRepository(database.appSettingsDao())
    private val chatRepository = ChatRepository(
        chatMessageDao = database.chatMessageDao(),
        sessionRepository = sessionRepository,
    )
    private val runRepository = RunRepository(
        agentRunDao = database.agentRunDao(),
        runEventDao = database.runEventDao(),
    )
    private val providerResolver: ChatProviderResolver = ChatProviderFactory(
        okHttpClient = OkHttpClient.Builder().build(),
    )

    val viewModelFactory: ViewModelProvider.Factory = RustAgentViewModelFactory(
        chatRepository = chatRepository,
        runRepository = runRepository,
        sessionRepository = sessionRepository,
        settingsRepository = settingsRepository,
        selectedSessionRepository = selectedSessionRepository,
        providerResolver = providerResolver,
    )
}

class RustAgentViewModelFactory(
    private val chatRepository: ChatRepository,
    private val runRepository: RunRepository,
    private val sessionRepository: SessionRepository,
    private val settingsRepository: SettingsRepository,
    private val selectedSessionRepository: SelectedSessionRepository,
    private val providerResolver: ChatProviderResolver,
) : ViewModelProvider.Factory {
    @Suppress("UNCHECKED_CAST")
    override fun <T : ViewModel> create(modelClass: Class<T>): T = when {
        modelClass.isAssignableFrom(ChatViewModel::class.java) -> {
            ChatViewModel(
                chatRepository = chatRepository,
                runRepository = runRepository,
                sessionRepository = sessionRepository,
                settingsRepository = settingsRepository,
                selectedSessionRepository = selectedSessionRepository,
                providerResolver = providerResolver,
            ) as T
        }

        modelClass.isAssignableFrom(SessionsViewModel::class.java) -> {
            SessionsViewModel(
                sessionRepository = sessionRepository,
                runRepository = runRepository,
                selectedSessionRepository = selectedSessionRepository,
            ) as T
        }

        modelClass.isAssignableFrom(SettingsViewModel::class.java) -> {
            SettingsViewModel(settingsRepository = settingsRepository) as T
        }

        else -> error("Unknown ViewModel class: ${modelClass.name}")
    }
}

