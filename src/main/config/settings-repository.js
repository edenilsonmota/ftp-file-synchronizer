const Store = require('electron-store');

const ENCRYPTED_PREFIX = 'safe-storage:';
const PLAINTEXT_PREFIX = 'plain-storage:';

class SettingsRepository {
    constructor({ safeStorage }) {
        this.safeStorage = safeStorage;
        this.store = new Store();
    }

    get() {
        const config = this.store.get('config', { projects: [] });
        const password = this.decryptPassword(config.password);
        if (password && !String(config.password).startsWith(ENCRYPTED_PREFIX)) {
            this.set({ projects: [], ...config, password });
        }
        return { ...config, password };
    }

    set(config) {
        const safeConfig = {
            ...config,
            password: this.encryptPassword(config.password),
            projects: config.projects.map(project => ({
                local: project.local,
                remote: project.remote
            }))
        };
        this.store.set('config', safeConfig);
    }

    encryptPassword(password = '') {
        const value = String(password || '');
        if (!value) return value;
        if (!this.safeStorage.isEncryptionAvailable()) {
            // Em algumas distribuições Linux não há um serviço Secret Service/keyring
            // disponível. Ainda assim, a configuração deve continuar utilizável entre
            // execuções; o prefixo torna explícito que este valor não foi cifrado.
            console.warn('[Config] Armazenamento seguro indisponível; a senha será salva sem criptografia.');
            return `${PLAINTEXT_PREFIX}${value}`;
        }
        return `${ENCRYPTED_PREFIX}${this.safeStorage.encryptString(value).toString('base64')}`;
    }

    decryptPassword(password = '') {
        const value = String(password || '');
        if (value.startsWith(PLAINTEXT_PREFIX)) return value.slice(PLAINTEXT_PREFIX.length);
        if (!value.startsWith(ENCRYPTED_PREFIX)) return value;
        try {
            const encrypted = Buffer.from(value.slice(ENCRYPTED_PREFIX.length), 'base64');
            return this.safeStorage.decryptString(encrypted);
        } catch (error) {
            console.error('[Config] Não foi possível descriptografar a senha:', error);
            return '';
        }
    }
}

module.exports = { SettingsRepository, ENCRYPTED_PREFIX, PLAINTEXT_PREFIX };
