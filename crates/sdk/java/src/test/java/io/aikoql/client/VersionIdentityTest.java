package io.aikoql.client;

import static org.junit.jupiter.api.Assertions.*;

import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Properties;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

import org.junit.jupiter.api.Test;

// V-02: the client identity is injected by the build (Maven resource
// filtering from pom.xml) — not restated as a literal next to the
// MIN_SERVER_VERSION contract constant (the literal drifted from pom.xml
// on every bump until someone remembered to edit the class).
class VersionIdentityTest {

    @Test
    void versionComesFromTheBuildArtifact() throws Exception {
        String built;
        try (InputStream in = AikoqlClient.class.getResourceAsStream("version.properties")) {
            assertNotNull(in, "version.properties must be filtered into the classes dir by Maven");
            var props = new Properties();
            props.load(in);
            built = props.getProperty("version", "");
        }
        assertFalse(built.isEmpty(), "the injected version must not be empty");
        assertFalse(built.startsWith("${"), "the resource was not filtered (a literal ${project.version} survived)");
        Path pom = Path.of(System.getProperty("basedir", "."), "pom.xml");
        Matcher m = Pattern.compile("<version>([^<]+)</version>").matcher(Files.readString(pom));
        assertTrue(m.find(), "pom.xml must carry a <version>");
        assertEquals(m.group(1), built, "the injected version must equal the pom version");
        assertEquals(built, AikoqlClient.VERSION, "the public constant must read the injected resource");
    }
}
