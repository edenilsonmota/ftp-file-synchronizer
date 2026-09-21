const assert = require('node:assert/strict');
const test = require('node:test');
const { SettingsRepository, ENCRYPTED_PREFIX, PLAINTEXT_PREFIX } = require('../src/main/config/settings-repository');

function repositoryWith(safeStorage) {
    return Object.assign(Object.create(SettingsRepository.prototype), { safeStorage });
}

test('persiste senha sem criptografia quando o armazenamento seguro não está disponível', () => {
    const repository = repositoryWith({ isEncryptionAvailable: () => false });
    const saved = repository.encryptPassword('minha-senha');

    assert.equal(saved, `${PLAINTEXT_PREFIX}minha-senha`);
    assert.equal(repository.decryptPassword(saved), 'minha-senha');
});

test('mantém senha cifrada quando o armazenamento seguro está disponível', () => {
    const repository = repositoryWith({
        isEncryptionAvailable: () => true,
        encryptString: value => Buffer.from(value),
        decryptString: value => value.toString()
    });

    const saved = repository.encryptPassword('minha-senha');
    assert.equal(saved, `${ENCRYPTED_PREFIX}${Buffer.from('minha-senha').toString('base64')}`);
    assert.equal(repository.decryptPassword(saved), 'minha-senha');
});

test('não confunde uma senha que começa com o marcador de armazenamento', () => {
    const repository = repositoryWith({
        isEncryptionAvailable: () => true,
        encryptString: value => Buffer.from(value),
        decryptString: value => value.toString()
    });

    const password = `${PLAINTEXT_PREFIX}minha-senha`;
    assert.equal(repository.decryptPassword(repository.encryptPassword(password)), password);
});
